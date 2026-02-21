#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::{
    io::IsTerminal,
    path::PathBuf,
    process::Stdio,
    rc::Rc,
    sync::{
        Arc,
        mpsc::{channel, sync_channel},
    },
};

use clap::Parser;
use floem::{
    IntoView, View, WindowIdExt,
    event::{Event, EventListener, EventPropagation},
    ext_event::{create_ext_action, create_signal_from_channel},
    peniko::{
        Gradient,
        kurbo::{Point, Size},
    },
    prelude::SignalTrack,
    reactive::{
        RwSignal, Scope, SignalGet, SignalUpdate, SignalWith, create_effect,
        create_rw_signal, provide_context, use_context,
    },
    style::CursorStyle,
    views::{
        Decorators, container, drag_resize_window_area, drag_window_area, empty,
        label, scroll, stack, svg,
    },
    window::{ResizeDirection, WindowConfig, WindowId},
};
use lapce_core::{
    directory::Directory,
    meta,
    syntax::{Syntax, highlight::reset_highlight_configs},
};
use lapce_rpc::{core::CoreNotification, file::PathObject};
use notify::Watcher;
use serde::{Deserialize, Serialize};
use tracing_subscriber::{filter::Targets, reload::Handle};

use crate::{
    about, alert,
    command::{InternalCommand, LapceWorkbenchCommand, WindowCommand},
    config::{
        LapceConfig, color::LapceColor, layout::LapceLayout, watcher::ConfigWatcher,
    },
    db::LapceDb,
    editor::location::{EditorLocation, EditorPosition},
    go_to_file, go_to_line, go_to_symbol,
    listener::Listener,
    panel::{position::PanelContainerPosition, view::panel_container_view},
    path::display_path,
    recent_files,
    status::status,
    title::title,
    tracing::*,
    update::ReleaseInfo,
    window::{TabsInfo, WindowData, WindowInfo},
    workspace::{LapceWorkspace, LapceWorkspaceType},
    workspace_data::WorkspaceData,
};

mod editor_tabs;
mod grammars;
mod ipc;
mod logging;
mod lsp_views;
mod menu;
mod ui_components;

pub use menu::window_menu;
use ui_components::tooltip_tip;
pub use ui_components::{
    clickable_icon, clickable_icon_base, not_clickable_icon, tooltip_label,
};

#[derive(Parser)]
#[clap(name = "SourceDelve")]
#[clap(version=meta::VERSION)]
#[derive(Debug)]
struct Cli {
    /// Launch new window even if SourceDelve is already running.
    /// Without this flag, SourceDelve tries to reuse an already-running instance via local socket.
    #[clap(short, long, action)]
    new: bool,
    /// Don't return instantly when opened in a terminal.
    /// When --wait is NOT set, the process re-spawns itself with --wait and exits immediately
    /// so the terminal prompt returns. The re-spawned child is the actual long-lived UI process.
    #[clap(short, long, action)]
    wait: bool,

    /// Paths to file(s) and/or folder(s) to open.
    /// When path is a file (that exists or not),
    /// it accepts `path:line:column` syntax
    /// to specify line and column at which it should open the file
    #[clap(value_parser = lapce_proxy::cli::parse_file_line_column)]
    #[clap(value_hint = clap::ValueHint::AnyPath)]
    paths: Vec<PathObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub windows: Vec<WindowInfo>,
}

/// Commands sent to the top-level application.
/// These are dispatched from windows/workspaces upward to AppData via the app_command Listener.
#[derive(Clone)]
pub enum AppCommand {
    SaveApp,
    NewWindow {
        folder: Option<PathBuf>,
    },
    CloseWindow(WindowId),
    WindowGotFocus(WindowId),
    /// Fired by the Floem WindowClosed event. Cleans up the window's scope/signals
    /// and persists app state. Distinguished from CloseWindow which initiates the close.
    WindowClosed(WindowId),
}

#[derive(Clone)]
pub struct AppData {
    pub windows: RwSignal<im::HashMap<WindowId, WindowData>>,
    pub active_window: RwSignal<WindowId>,
    pub window_scale: RwSignal<f64>,
    pub app_command: Listener<AppCommand>,
    pub app_terminated: RwSignal<bool>,
    /// The latest release information
    pub latest_release: RwSignal<Arc<Option<ReleaseInfo>>>,
    pub watcher: Arc<notify::RecommendedWatcher>,
    pub tracing_handle: Handle<Targets>,
    pub config: RwSignal<Arc<LapceConfig>>,
}

impl AppData {
    pub fn reload_config(&self) {
        let config = LapceConfig::load(&LapceWorkspace::default());

        self.config.set(Arc::new(config));
        self.window_scale.set(self.config.get().ui.scale());

        let windows = self.windows.get_untracked();
        for (_, window) in windows {
            window.reload_config();
        }
    }

    pub fn active_workspace(&self) -> Option<Rc<WorkspaceData>> {
        if let Some(window) = self.active_window() {
            return window.active_workspace();
        }
        None
    }

    fn active_window(&self) -> Option<WindowData> {
        let windows = self.windows.get_untracked();
        let active_window = self.active_window.get_untracked();
        windows
            .get(&active_window)
            .cloned()
            .or_else(|| windows.iter().next().map(|(_, window)| window.clone()))
    }

    /// Base window configuration. Disables Floem's default theme because Lapce
    /// applies its own color theme system from config files and plugins.
    fn default_window_config(&self) -> WindowConfig {
        WindowConfig::default()
            .apply_default_theme(false)
            .title("SourceDelve")
    }

    pub fn new_window(&self, folder: Option<PathBuf>) {
        let config = self
            .active_window()
            .map(|window| {
                self.default_window_config()
                    .size(window.common.size.get_untracked())
                    .position(window.position.get_untracked() + (50.0, 50.0))
            })
            .or_else(|| {
                let db: Arc<LapceDb> =
                    use_context().expect("LapceDb must be provided as context");
                db.get_window().ok().map(|info| {
                    self.default_window_config()
                        .size(info.size)
                        .position(info.pos)
                })
            })
            .unwrap_or_else(|| {
                self.default_window_config().size(Size::new(
                    LapceLayout::DEFAULT_WINDOW_WIDTH,
                    LapceLayout::DEFAULT_WINDOW_HEIGHT,
                ))
            });
        let config = if cfg!(target_os = "macos")
            || self.config.get_untracked().core.custom_titlebar
        {
            config.show_titlebar(false)
        } else {
            config
        };
        let workspace = LapceWorkspace {
            path: folder,
            ..Default::default()
        };
        let app_data = self.clone();
        floem::new_window(
            move |window_id| {
                app_data.app_view(
                    window_id,
                    WindowInfo {
                        size: Size::ZERO,
                        pos: Point::ZERO,
                        maximised: false,
                        tabs: TabsInfo {
                            active_tab: 0,
                            workspaces: vec![workspace],
                        },
                    },
                    vec![],
                )
            },
            Some(config),
        );
    }

    pub fn run_app_command(&self, cmd: AppCommand) {
        match cmd {
            AppCommand::SaveApp => {
                let db: Arc<LapceDb> =
                    use_context().expect("LapceDb must be provided as context");
                if let Err(err) = db.save_app(self) {
                    tracing::error!("{:?}", err);
                }
            }
            AppCommand::WindowClosed(window_id) => {
                if self.app_terminated.get_untracked() {
                    return;
                }
                let db: Arc<LapceDb> =
                    use_context().expect("LapceDb must be provided as context");
                let is_last = self.windows.with_untracked(|w| w.len()) == 1;
                if is_last {
                    if let Err(err) = db.insert_app(self.clone()) {
                        tracing::error!("{:?}", err);
                    }
                }
                if let Some(window_data) = self
                    .windows
                    .try_update(|windows| windows.remove(&window_id))
                    .flatten()
                {
                    window_data.scope.dispose();
                }
                if let Err(err) = db.save_app(self) {
                    tracing::error!("{:?}", err);
                }
                // Always keep at least one window open. When the user closes
                // the last window, reopen the empty workspace landing page.
                // To actually quit, use Cmd+Q / the Quit menu item.
                if is_last {
                    self.new_window(None);
                }
            }
            AppCommand::CloseWindow(window_id) => {
                floem::close_window(window_id);
            }
            AppCommand::NewWindow { folder } => {
                self.new_window(folder);
            }
            AppCommand::WindowGotFocus(window_id) => {
                self.active_window.set(window_id);
            }
        }
    }

    /// Determine which OS windows to create based on CLI arguments and persisted state.
    /// Priority: (1) directories from CLI -> one window per dir, (2) no args -> restore
    /// from last session, (3) fallback -> single empty window. Files from CLI are opened
    /// in the first directory window, or in the fallback window if no dirs were specified.
    fn create_windows(
        &self,
        db: Arc<LapceDb>,
        paths: Vec<PathObject>,
    ) -> floem::Application {
        let mut app = floem::Application::new_with_config(
            floem::AppConfig::default().exit_on_close(false),
        );

        let mut initial_windows = 0;

        // Split user input into known existing directors and
        // file paths that exist or not
        let (dirs, files): (Vec<&PathObject>, Vec<&PathObject>) =
            paths.iter().partition(|p| p.is_dir);

        let files: Vec<PathObject> = files.into_iter().cloned().collect();
        let mut files = if files.is_empty() { None } else { Some(files) };

        if !dirs.is_empty() {
            // There were directories specified, so we'll load those as windows

            // Use the last opened window's size and position as the default
            let (size, mut pos) = db
                .get_window()
                .map(|i| (i.size, i.pos))
                .unwrap_or_else(|_| {
                    (
                        Size::new(
                            LapceLayout::DEFAULT_WINDOW_WIDTH,
                            LapceLayout::DEFAULT_WINDOW_HEIGHT,
                        ),
                        Point::new(0.0, 0.0),
                    )
                });

            for dir in dirs {
                let workspace_type = LapceWorkspaceType::Local;

                let info = WindowInfo {
                    size,
                    pos,
                    maximised: false,
                    tabs: TabsInfo {
                        active_tab: 0,
                        workspaces: vec![LapceWorkspace {
                            kind: workspace_type,
                            path: Some(dir.path.to_owned()),
                            last_open: 0,
                        }],
                    },
                };

                pos += (50.0, 50.0);

                let config = self
                    .default_window_config()
                    .size(info.size)
                    .position(info.pos);
                let config = if cfg!(target_os = "macos")
                    || self.config.get_untracked().core.custom_titlebar
                {
                    config.show_titlebar(false)
                } else {
                    config
                };
                let app_data = self.clone();
                let files = files.take().unwrap_or_default();
                app = app.window(
                    move |window_id| app_data.app_view(window_id, info, files),
                    Some(config),
                );
                initial_windows += 1;
            }
        } else if files.is_none() {
            // There were no dirs and no files specified, so we'll load the last windows
            match db.get_app() {
                Ok(app_info) => {
                    for info in app_info.windows {
                        let config = self
                            .default_window_config()
                            .size(info.size)
                            .position(info.pos);
                        let config = if cfg!(target_os = "macos")
                            || self.config.get_untracked().core.custom_titlebar
                        {
                            config.show_titlebar(false)
                        } else {
                            config
                        };
                        let app_data = self.clone();
                        app = app.window(
                            move |window_id| {
                                app_data.app_view(window_id, info, vec![])
                            },
                            Some(config),
                        );
                        initial_windows += 1;
                    }
                }
                Err(err) => {
                    tracing::error!("{:?}", err);
                }
            }
        }

        if initial_windows == 0 {
            let mut info = db.get_window().unwrap_or_else(|_| WindowInfo {
                size: Size::new(
                    LapceLayout::DEFAULT_WINDOW_WIDTH,
                    LapceLayout::DEFAULT_WINDOW_HEIGHT,
                ),
                pos: Point::ZERO,
                maximised: false,
                tabs: TabsInfo {
                    active_tab: 0,
                    workspaces: vec![LapceWorkspace::default()],
                },
            });
            info.tabs = TabsInfo {
                active_tab: 0,
                workspaces: vec![LapceWorkspace::default()],
            };
            let config = self
                .default_window_config()
                .size(info.size)
                .position(info.pos);
            let config = if cfg!(target_os = "macos")
                || self.config.get_untracked().core.custom_titlebar
            {
                config.show_titlebar(false)
            } else {
                config
            };
            let app_data = self.clone();
            app = app.window(
                move |window_id| {
                    app_data.app_view(
                        window_id,
                        info,
                        files.take().unwrap_or_default(),
                    )
                },
                Some(config),
            );
        }

        app
    }

    fn app_view(
        &self,
        window_id: WindowId,
        info: WindowInfo,
        files: Vec<PathObject>,
    ) -> impl View + use<> {
        let app_view_id = create_rw_signal(floem::ViewId::new());
        let window_data = WindowData::new(
            window_id,
            app_view_id,
            info,
            self.window_scale,
            self.latest_release.read_only(),
            self.app_command,
        );

        {
            let workspace = &window_data.workspace;
            for file in files {
                let position = file.linecol.map(|pos| {
                    EditorPosition::Position(lsp_types::Position {
                        line: pos.line.saturating_sub(1) as u32,
                        character: pos.column.saturating_sub(1) as u32,
                    })
                });

                workspace.run_internal_command(InternalCommand::GoToLocation {
                    location: EditorLocation {
                        path: file.path.clone(),
                        position,
                        scroll_offset: None,
                        same_editor_tab: false,
                    },
                });
            }
        }

        self.windows.update(|windows| {
            windows.insert(window_id, window_data.clone());
        });
        let window_size = window_data.common.size;
        let position = window_data.position;
        let window_scale = window_data.window_scale;
        let app_command = window_data.app_command;
        let config = window_data.config;
        // The KeyDown and PointerDown event handlers both need ownership of a WindowData object.
        let key_down_window_data = window_data.clone();
        // The top-level view is a stack of the actual window content and invisible drag-resize
        // areas around the edges. These resize areas implement custom window chrome for platforms
        // (non-macOS) where we hide the native titlebar. They are 4px wide/tall edge strips
        // and 20px corner zones, positioned absolutely to overlay the window edges.
        let view = stack((
            window(window_data.clone()),
            stack((
                drag_resize_window_area(ResizeDirection::West, empty()).style(|s| {
                    s.absolute().width(4.0).height_full().pointer_events_auto()
                }),
                drag_resize_window_area(ResizeDirection::North, empty()).style(
                    |s| s.absolute().width_full().height(4.0).pointer_events_auto(),
                ),
                drag_resize_window_area(ResizeDirection::East, empty()).style(
                    move |s| {
                        s.absolute()
                            .margin_left(window_size.get().width as f32 - 4.0)
                            .width(4.0)
                            .height_full()
                            .pointer_events_auto()
                    },
                ),
                drag_resize_window_area(ResizeDirection::South, empty()).style(
                    move |s| {
                        s.absolute()
                            .margin_top(window_size.get().height as f32 - 4.0)
                            .width_full()
                            .height(4.0)
                            .pointer_events_auto()
                    },
                ),
                drag_resize_window_area(ResizeDirection::NorthWest, empty()).style(
                    |s| s.absolute().width(20.0).height(4.0).pointer_events_auto(),
                ),
                drag_resize_window_area(ResizeDirection::NorthWest, empty()).style(
                    |s| s.absolute().width(4.0).height(20.0).pointer_events_auto(),
                ),
                drag_resize_window_area(ResizeDirection::NorthEast, empty()).style(
                    move |s| {
                        s.absolute()
                            .margin_left(window_size.get().width as f32 - 20.0)
                            .width(20.0)
                            .height(4.0)
                            .pointer_events_auto()
                    },
                ),
                drag_resize_window_area(ResizeDirection::NorthEast, empty()).style(
                    move |s| {
                        s.absolute()
                            .margin_left(window_size.get().width as f32 - 4.0)
                            .width(4.0)
                            .height(20.0)
                            .pointer_events_auto()
                    },
                ),
                drag_resize_window_area(ResizeDirection::SouthWest, empty()).style(
                    move |s| {
                        s.absolute()
                            .margin_top(window_size.get().height as f32 - 4.0)
                            .width(20.0)
                            .height(4.0)
                            .pointer_events_auto()
                    },
                ),
                drag_resize_window_area(ResizeDirection::SouthWest, empty()).style(
                    move |s| {
                        s.absolute()
                            .margin_top(window_size.get().height as f32 - 20.0)
                            .width(4.0)
                            .height(20.0)
                            .pointer_events_auto()
                    },
                ),
                drag_resize_window_area(ResizeDirection::SouthEast, empty()).style(
                    move |s| {
                        s.absolute()
                            .margin_left(window_size.get().width as f32 - 20.0)
                            .margin_top(window_size.get().height as f32 - 4.0)
                            .width(20.0)
                            .height(4.0)
                            .pointer_events_auto()
                    },
                ),
                drag_resize_window_area(ResizeDirection::SouthEast, empty()).style(
                    move |s| {
                        s.absolute()
                            .margin_left(window_size.get().width as f32 - 4.0)
                            .margin_top(window_size.get().height as f32 - 20.0)
                            .width(4.0)
                            .height(20.0)
                            .pointer_events_auto()
                    },
                ),
            ))
            .debug_name("Drag Resize Areas")
            .style(move |s| {
                s.absolute()
                    .size_full()
                    .apply_if(
                        cfg!(target_os = "macos")
                            || !config.get_untracked().core.custom_titlebar,
                        |s| s.hide(),
                    )
                    .pointer_events_none()
            }),
        ))
        .style(|s| s.flex_col().size_full());
        let view_id = view.id();
        app_view_id.set(view_id);

        view_id.request_focus();

        // All keyboard and pointer events are captured at this top-level view and routed
        // through WindowData::key_down, which dispatches to the active workspace's focus system.
        // This is the single entry point for the entire keyboard routing pipeline.
        view.window_scale(move || window_scale.get())
            .keyboard_navigable()
            .on_event(EventListener::KeyDown, move |event| {
                if let Event::KeyDown(key_event) = event {
                    if key_down_window_data.key_down(key_event) {
                        view_id.request_focus();
                    }
                    EventPropagation::Stop
                } else {
                    EventPropagation::Continue
                }
            })
            .on_event(EventListener::PointerDown, {
                let window_data = window_data.clone();
                move |event| {
                    if let Event::PointerDown(pointer_event) = event {
                        window_data.key_down(pointer_event);
                        EventPropagation::Stop
                    } else {
                        EventPropagation::Continue
                    }
                }
            })
            .on_event_stop(EventListener::WindowResized, move |event| {
                if let Event::WindowResized(size) = event {
                    window_size.set(*size);
                }
            })
            .on_event_stop(EventListener::WindowMoved, move |event| {
                if let Event::WindowMoved(point) = event {
                    position.set(*point);
                }
            })
            .on_event_stop(EventListener::WindowGotFocus, move |_| {
                app_command.send(AppCommand::WindowGotFocus(window_id));
            })
            .on_event_stop(EventListener::WindowClosed, move |_| {
                app_command.send(AppCommand::WindowClosed(window_id));
            })
            .on_event_stop(EventListener::DroppedFile, move |event: &Event| {
                if let Event::DroppedFile(file) = event {
                    if file.path.is_dir() {
                        app_command.send(AppCommand::NewWindow {
                            folder: Some(file.path.clone()),
                        });
                    } else if let Some(win_tab_data) = window_data.active_workspace()
                    {
                        win_tab_data.common.internal_command.send(
                            InternalCommand::GoToLocation {
                                location: EditorLocation {
                                    path: file.path.clone(),
                                    position: None,
                                    scroll_offset: None,

                                    same_editor_tab: false,
                                },
                            },
                        )
                    }
                }
            })
            .debug_name("App View")
    }
}

/// The main workbench layout: a vertical stack containing:
///   1. A horizontal row of [left panel | main editor split | right panel]
///   2. The bottom panel (search, problems, etc.)
///   3. Window-level message notifications (LSP errors, warnings)
///
/// The horizontal row uses flex-grow so the editor area fills remaining space
/// after the side panels. The bottom panel sits below and can be maximized
/// to hide the editor area entirely.
fn workbench(workspace_data: Rc<WorkspaceData>) -> impl View {
    let workbench_size = workspace_data.common.workbench_size;
    let main_split_width = workspace_data.main_split.width;
    stack((
        {
            let workspace_data = workspace_data.clone();
            stack((
                panel_container_view(
                    workspace_data.clone(),
                    PanelContainerPosition::Left,
                ),
                editor_tabs::main_split(workspace_data.clone()),
                panel_container_view(workspace_data, PanelContainerPosition::Right),
            ))
            .on_resize(move |rect| {
                let width = rect.size().width;
                if main_split_width.get_untracked() != width {
                    main_split_width.set(width);
                }
            })
            .style(|s| s.flex_grow(1.0).gap(6.0))
        },
        panel_container_view(workspace_data.clone(), PanelContainerPosition::Bottom),
        lsp_views::window_message_view(
            workspace_data.messages,
            workspace_data.common.config,
        ),
    ))
    .on_resize(move |rect| {
        let size = rect.size();
        if size != workbench_size.get_untracked() {
            workbench_size.set(size);
        }
    })
    .style(move |s| s.flex_col().size_full().padding(8.0).gap(6.0))
    .debug_name("Workbench")
}

fn empty_workspace_view(workspace_data: Rc<WorkspaceData>) -> impl View {
    let config = workspace_data.common.config;
    let workbench_command = workspace_data.common.workbench_command;
    let window_command = workspace_data.common.window_common.window_command;

    let db: Arc<LapceDb> =
        use_context().expect("LapceDb must be provided as context");
    let recent_workspaces: Vec<_> = db
        .recent_workspaces()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|ws| {
            let path = ws.path.clone()?;
            Some((ws, path))
        })
        .take(8)
        .collect();

    let logo = svg(move || config.get().logo_svg()).style(move |s| {
        s.size(64.0, 64.0).color(
            config
                .get()
                .color(LapceColor::EDITOR_FOREGROUND)
                .multiply_alpha(0.15),
        )
    });

    let section_label = |text: &'static str| {
        let config = config;
        label(move || text.to_string()).style(move |s| {
            s.font_bold()
                .font_size((config.get().ui.font_size() - 1) as f32)
                .color(config.get().color(LapceColor::EDITOR_DIM))
                .margin_bottom(8.0)
        })
    };

    let open_folder_btn = label(move || "Open Workspace".to_string())
        .on_event_stop(EventListener::PointerDown, |_| {})
        .on_click_stop(move |_| {
            workbench_command.send(LapceWorkbenchCommand::OpenFolder);
        })
        .style(move |s| {
            let config = config.get();
            s.padding_horiz(20.0)
                .padding_vert(8.0)
                .border_radius(LapceLayout::BORDER_RADIUS)
                .border(1.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .color(config.color(LapceColor::EDITOR_FOREGROUND))
                .hover(|s| {
                    s.cursor(CursorStyle::Pointer).background(
                        config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                    )
                })
                .active(|s| {
                    s.background(
                        config.color(LapceColor::PANEL_HOVERED_ACTIVE_BACKGROUND),
                    )
                })
        });

    let actions = stack((section_label("Start"), open_folder_btn))
        .style(|s| s.flex_col().items_start());

    let recent_section = if recent_workspaces.is_empty() {
        container(empty()).into_any()
    } else {
        let items = recent_workspaces
            .into_iter()
            .map(|(ws, path)| {
                let name = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .to_string();
                let folder = display_path(&path);
                let window_command = window_command;
                let ws_clone = ws.clone();

                stack((
                    label(move || name.clone()).style(move |s| {
                        s.color(config.get().color(LapceColor::EDITOR_FOREGROUND))
                    }),
                    label(move || folder.clone()).style(move |s| {
                        s.color(config.get().color(LapceColor::EDITOR_DIM))
                            .font_size((config.get().ui.font_size() - 2) as f32)
                            .margin_top(2.0)
                    }),
                ))
                .on_event_stop(EventListener::PointerDown, |_| {})
                .on_click_stop(move |_| {
                    window_command.send(WindowCommand::SetWorkspace {
                        workspace: ws_clone.clone(),
                    });
                })
                .style(move |s| {
                    let config = config.get();
                    s.flex_col()
                        .width_full()
                        .padding_horiz(10.0)
                        .padding_vert(6.0)
                        .border_radius(LapceLayout::BORDER_RADIUS)
                        .hover(|s| {
                            s.cursor(CursorStyle::Pointer).background(
                                config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                        })
                        .active(|s| {
                            s.background(
                                config.color(
                                    LapceColor::PANEL_HOVERED_ACTIVE_BACKGROUND,
                                ),
                            )
                        })
                })
            })
            .collect::<Vec<_>>();

        stack((
            section_label("Recent"),
            scroll(
                floem::views::stack_from_iter(items)
                    .style(|s| s.flex_col().width_full()),
            )
            .style(|s| s.width_full().max_height(300.0)),
        ))
        .style(|s| s.flex_col().items_start().width_full())
        .into_any()
    };

    let content = stack((logo, actions, recent_section)).style(|s| {
        s.flex_col()
            .items_center()
            .gap(25.0)
            .max_width(380.0)
            .width_full()
    });

    drag_window_area(
        container(content)
            .style(|s| s.size_full().flex_col().items_center().justify_center()),
    )
}

/// The full view for a single workspace tab. This is the root of the per-workspace UI tree.
/// It uses a layered stack where the base layer (title + workbench + status bar) is overlaid
/// by floating elements in z-order: completion, hover, code actions, rename,
/// go-to-file, search modal, recent files, about popup, and alert dialog.
/// If no folder is open, shows a simplified "Open Workspace" landing page instead.
fn workspace_view(workspace_data: Rc<WorkspaceData>) -> impl View {
    let window_origin = workspace_data.common.window_origin;
    let layout_rect = workspace_data.layout_rect;
    let config = workspace_data.common.config;
    let workspace_scope = workspace_data.scope;
    let hover_active = workspace_data.common.hover.active;
    let window_id = workspace_data.common.window_common.window_id;

    let view = if workspace_data.workspace.path.is_none() {
        empty_workspace_view(workspace_data.clone()).into_any()
    } else {
        let status_height = workspace_data.status_height;
        stack((
            stack((
                title(workspace_data.clone()),
                workbench(workspace_data.clone()),
                status(workspace_data.clone(), status_height),
            ))
            .on_resize(move |rect| {
                layout_rect.set(rect);
            })
            .on_move(move |point| {
                window_origin.set(point);
            })
            .style(|s| s.size_full().flex_col())
            .debug_name("Base Layer"),
            crate::status::background_tasks_popup(workspace_data.clone()),
            lsp_views::completion(workspace_data.clone()),
            lsp_views::hover(workspace_data.clone()),
            lsp_views::code_action(workspace_data.clone()),
            lsp_views::definition_picker(workspace_data.clone()),
            lsp_views::rename(workspace_data.clone()),
            go_to_file::go_to_file_popup(workspace_data.clone()),
            crate::search_modal::search_modal_popup(workspace_data.clone()),
            crate::replace_modal::replace_modal_popup(workspace_data.clone()),
            recent_files::recent_files_popup(workspace_data.clone()),
            go_to_line::go_to_line_popup(workspace_data.clone()),
            go_to_symbol::go_to_symbol_popup(workspace_data.clone()),
            about::about_popup(workspace_data.clone()),
            alert::alert_box(workspace_data.alert_data.clone()),
        ))
        .into_any()
    };

    let view = view
        .on_cleanup(move || {
            workspace_scope.dispose();
        })
        .on_event_cont(EventListener::PointerMove, move |_| {
            if hover_active.get_untracked() {
                hover_active.set(false);
            }
        })
        .style(move |s| {
            let config = config.get();
            let scale = window_id.scale();
            let gradient = Gradient::new_linear(
                Point::new(0.0, 0.0),
                Point::new(1200.0 * scale, 900.0 * scale),
            )
            .with_stops([
                (0.0, config.color(LapceColor::SHELL_BACKGROUND_TOP)),
                (0.4, config.color(LapceColor::SHELL_BACKGROUND)),
                (1.0, config.color(LapceColor::SHELL_BACKGROUND)),
            ]);
            s.size_full()
                .color(config.color(LapceColor::EDITOR_FOREGROUND))
                .background(gradient)
                .font_size(config.ui.font_size() as f32)
                .apply_if(!config.ui.font_family.is_empty(), |s| {
                    s.font_family(config.ui.font_family.clone())
                })
                .class(floem::views::scroll::Handle, |s| {
                    s.background(config.color(LapceColor::LAPCE_SCROLL_BAR))
                })
        })
        .debug_name("Workspace");

    let view_id = view.id();
    workspace_data.common.view_id.set(view_id);
    view
}

fn window(window_data: WindowData) -> impl View {
    let workspace_data = window_data.workspace.clone();
    let ime_enabled = window_data.ime_enabled;
    let window_maximized = window_data.common.window_maximized;

    workspace_view(workspace_data.clone())
        .window_title(move || {
            let workspace_name = workspace_data.workspace.display();
            let branch = workspace_data.git_branch.get();
            let repo_state_label = workspace_data
                .git_repo_state
                .get()
                .label()
                .map(String::from);
            let branch_display = match (branch, repo_state_label) {
                (Some(br), Some(state)) => Some(format!("{br} ({state})")),
                (Some(br), None) => Some(br),
                _ => None,
            };
            match (workspace_name, branch_display) {
                (Some(ws), Some(br)) => format!("{ws} [{br}] - SourceDelve"),
                (Some(ws), None) => format!("{ws} - SourceDelve"),
                _ => "SourceDelve".to_string(),
            }
        })
        .on_event_stop(EventListener::ImeEnabled, move |_| {
            ime_enabled.set(true);
        })
        .on_event_stop(EventListener::ImeDisabled, move |_| {
            ime_enabled.set(false);
        })
        .on_event_cont(EventListener::WindowMaximizeChanged, move |event| {
            if let Event::WindowMaximizeChanged(maximized) = event {
                window_maximized.set(*maximized);
            }
        })
        .window_menu(move || {
            let workspace = &window_data.workspace;
            workspace.common.keypress.track();
            let workbench_command = workspace.common.workbench_command;
            let lapce_command = workspace.common.lapce_command;
            let window_command = workspace.common.window_common.window_command;
            window_menu(
                lapce_command,
                workbench_command,
                window_command,
                &workspace.workspace,
            )
        })
        .style(|s| s.size_full())
        .debug_name("Window")
}

/// Application entry point. Orchestrates the entire startup sequence:
/// 1. Parse CLI args
/// 2. Set up logging and panic hooks
/// 3. Load vendored fonts (if feature enabled)
/// 4. Load shell environment (for non-terminal launches, e.g. from dock/Finder)
/// 5. Re-spawn as background process if not --wait (for terminal detach)
/// 6. Try to connect to existing Lapce instance via local socket (single-instance)
/// 7. Initialize database, config, file watchers, bundled plugins
/// 8. Create windows from CLI paths or restored session
/// 9. Spawn background threads for grammar updates, release checks, socket listener
/// 10. Run the Floem event loop
pub fn launch() {
    let cli = Cli::parse();

    if !cli.wait {
        logging::panic_hook();
    }

    let (reload_handle, _guard) = logging::logging();
    trace!(TraceLevel::INFO, "Starting up Lapce..");

    #[cfg(feature = "vendored-fonts")]
    {
        use floem::text::{FONT_SYSTEM, fontdb::Source};

        const FONT_DEJAVU_SANS_REGULAR: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../extra/fonts/DejaVu/DejaVuSans.ttf"
        ));
        const FONT_DEJAVU_SANS_MONO_REGULAR: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../extra/fonts/DejaVu/DejaVuSansMono.ttf"
        ));

        macro_rules! inter_font {
            ($path:literal) => {
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../extra/fonts/Inter/",
                    $path
                ))
            };
        }

        const INTER_THIN: &[u8] = inter_font!("Inter-Thin.ttf");
        const INTER_THIN_ITALIC: &[u8] = inter_font!("Inter-ThinItalic.ttf");
        const INTER_EXTRA_LIGHT: &[u8] = inter_font!("Inter-ExtraLight.ttf");
        const INTER_EXTRA_LIGHT_ITALIC: &[u8] =
            inter_font!("Inter-ExtraLightItalic.ttf");
        const INTER_LIGHT: &[u8] = inter_font!("Inter-Light.ttf");
        const INTER_LIGHT_ITALIC: &[u8] = inter_font!("Inter-LightItalic.ttf");
        const INTER_REGULAR: &[u8] = inter_font!("Inter-Regular.ttf");
        const INTER_ITALIC: &[u8] = inter_font!("Inter-Italic.ttf");
        const INTER_MEDIUM: &[u8] = inter_font!("Inter-Medium.ttf");
        const INTER_MEDIUM_ITALIC: &[u8] = inter_font!("Inter-MediumItalic.ttf");
        const INTER_SEMI_BOLD: &[u8] = inter_font!("Inter-SemiBold.ttf");
        const INTER_SEMI_BOLD_ITALIC: &[u8] =
            inter_font!("Inter-SemiBoldItalic.ttf");
        const INTER_BOLD: &[u8] = inter_font!("Inter-Bold.ttf");
        const INTER_BOLD_ITALIC: &[u8] = inter_font!("Inter-BoldItalic.ttf");
        const INTER_EXTRA_BOLD: &[u8] = inter_font!("Inter-ExtraBold.ttf");
        const INTER_EXTRA_BOLD_ITALIC: &[u8] =
            inter_font!("Inter-ExtraBoldItalic.ttf");
        const INTER_BLACK: &[u8] = inter_font!("Inter-Black.ttf");
        const INTER_BLACK_ITALIC: &[u8] = inter_font!("Inter-BlackItalic.ttf");

        let mut font_db = FONT_SYSTEM.lock();
        let db = font_db.db_mut();

        db.load_font_source(Source::Binary(Arc::new(FONT_DEJAVU_SANS_REGULAR)));
        db.load_font_source(Source::Binary(Arc::new(FONT_DEJAVU_SANS_MONO_REGULAR)));

        for font_data in [
            INTER_THIN,
            INTER_THIN_ITALIC,
            INTER_EXTRA_LIGHT,
            INTER_EXTRA_LIGHT_ITALIC,
            INTER_LIGHT,
            INTER_LIGHT_ITALIC,
            INTER_REGULAR,
            INTER_ITALIC,
            INTER_MEDIUM,
            INTER_MEDIUM_ITALIC,
            INTER_SEMI_BOLD,
            INTER_SEMI_BOLD_ITALIC,
            INTER_BOLD,
            INTER_BOLD_ITALIC,
            INTER_EXTRA_BOLD,
            INTER_EXTRA_BOLD_ITALIC,
            INTER_BLACK,
            INTER_BLACK_ITALIC,
        ] {
            db.load_font_source(Source::Binary(Arc::new(font_data)));
        }
    }

    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        trace!(TraceLevel::INFO, "Loading custom environment from shell");
        ipc::load_shell_env();
    }

    // When launched from a terminal without --wait, re-spawn ourselves as a detached child
    // with --wait so the parent process can exit immediately and return the shell prompt.
    // The child inherits our args plus --wait, with stdout/stderr redirected to log files.
    if !cli.wait {
        let mut args = std::env::args().collect::<Vec<_>>();
        args.push("--wait".to_string());
        let mut cmd = std::process::Command::new(&args[0]);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW);

        let (stderr, stdout) = if let Some(logs_dir) = Directory::logs_directory() {
            let stderr_file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .read(true)
                .open(logs_dir.join("stderr.log"))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::inherit());
            let stdout_file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .read(true)
                .open(logs_dir.join("stdout.log"))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::inherit());
            (stderr_file, stdout_file)
        } else {
            (Stdio::inherit(), Stdio::inherit())
        };

        if let Err(why) = cmd
            .args(&args[1..])
            .stderr(stderr)
            .stdout(stdout)
            .env("SOURCEDELVE_LOG", "lapce_app::app=error,off")
            .spawn()
        {
            eprintln!("Failed to launch sourcedelve: {why}");
            std::process::exit(1);
        };
        return;
    }

    // Single-instance behavior: try to send the paths to an already-running Lapce via
    // local socket. If successful, exit this process. If the socket doesn't exist or the
    // connection fails, we become the primary instance and proceed with full initialization.
    if !cli.new {
        match ipc::get_socket() {
            Ok(socket) => {
                if let Err(e) = ipc::try_open_in_existing_process(socket, &cli.paths)
                {
                    trace!(TraceLevel::ERROR, "failed to open path(s): {e}");
                };
                return;
            }
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }
    }

    #[cfg(feature = "updater")]
    crate::update::cleanup();

    if let Err(err) = lapce_proxy::register_lapce_path() {
        tracing::error!("{:?}", err);
    }
    let db = match LapceDb::new() {
        Ok(db) => Arc::new(db),
        Err(e) => {
            #[cfg(windows)]
            logging::error_modal("Error", &format!("Failed to create LapceDb: {e}"));

            trace!(TraceLevel::ERROR, "Failed to create LapceDb: {e}");
            std::process::exit(1);
        }
    };
    let scope = Scope::new();
    provide_context(db.clone());

    let window_scale = scope.create_rw_signal(1.0);
    let latest_release = scope.create_rw_signal(Arc::new(None));
    let app_command = Listener::new_empty(scope);

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(ConfigWatcher::new(tx)).unwrap();
    if let Some(path) = LapceConfig::settings_file() {
        if let Err(err) = watcher.watch(&path, notify::RecursiveMode::Recursive) {
            tracing::error!("{:?}", err);
        }
    }
    if let Some(path) = LapceConfig::keymaps_file() {
        if let Err(err) = watcher.watch(&path, notify::RecursiveMode::Recursive) {
            tracing::error!("{:?}", err);
        }
    }
    let windows = scope.create_rw_signal(im::HashMap::new());
    let config = LapceConfig::load(&LapceWorkspace::default());

    // Restore scale from config
    window_scale.set(config.ui.scale());

    let config = scope.create_rw_signal(Arc::new(config));
    let app_data = AppData {
        windows,
        active_window: scope.create_rw_signal(WindowId::from_raw(0)),
        window_scale,
        app_terminated: scope.create_rw_signal(false),
        watcher: Arc::new(watcher),
        latest_release,
        app_command,
        tracing_handle: reload_handle,
        config,
    };

    let app = app_data.create_windows(db.clone(), cli.paths);

    {
        let app_data = app_data.clone();
        let notification = create_signal_from_channel(rx);
        create_effect(move |_| {
            if notification.get().is_some() {
                tracing::debug!("notification reload_config");
                app_data.reload_config();
            }
        });
    }

    {
        let cx = Scope::new();
        let app_data = app_data.clone();
        let send = create_ext_action(cx, move |updated| {
            if updated {
                trace!(
                    TraceLevel::INFO,
                    "grammar or query got updated, reset highlight configs"
                );
                reset_highlight_configs();
                for (_, window) in app_data.windows.get_untracked() {
                    for (_, doc) in window.workspace.main_split.docs.get_untracked()
                    {
                        doc.syntax.update(|syntaxt| {
                            *syntaxt = Syntax::from_language(syntaxt.language);
                        });
                        doc.trigger_syntax_change(None);
                    }
                }
            }
        });
        std::thread::Builder::new()
            .name("FindGrammar".to_owned())
            .spawn(move || {
                use self::grammars::*;
                let updated = match find_grammar_release() {
                    Ok(release) => {
                        let mut updated = false;
                        match fetch_grammars(&release) {
                            Err(e) => {
                                trace!(
                                    TraceLevel::ERROR,
                                    "failed to fetch grammars: {e}"
                                );
                            }
                            Ok(u) => updated |= u,
                        }
                        match fetch_queries(&release) {
                            Err(e) => {
                                trace!(
                                    TraceLevel::ERROR,
                                    "failed to fetch grammars: {e}"
                                );
                            }
                            Ok(u) => updated |= u,
                        }
                        updated
                    }
                    Err(e) => {
                        trace!(
                            TraceLevel::ERROR,
                            "failed to obtain release info: {e}"
                        );
                        false
                    }
                };
                send(updated);
            })
            .unwrap();
    }

    #[cfg(feature = "updater")]
    {
        let (tx, rx) = sync_channel(1);
        let notification = create_signal_from_channel(rx);
        let latest_release = app_data.latest_release;
        create_effect(move |_| {
            if let Some(release) = notification.get() {
                latest_release.set(Arc::new(Some(release)));
            }
        });
        std::thread::Builder::new()
            .name("LapceUpdater".to_owned())
            .spawn(move || {
                loop {
                    if let Ok(release) = crate::update::get_latest_release() {
                        if let Err(err) = tx.send(release) {
                            tracing::error!("{:?}", err);
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(60 * 60));
                }
            })
            .unwrap();
    }

    {
        let (tx, rx) = sync_channel(1);
        let notification = create_signal_from_channel(rx);
        let app_data = app_data.clone();
        create_effect(move |_| {
            if let Some(CoreNotification::OpenPaths { paths }) = notification.get() {
                if let Some(workspace) = app_data.active_workspace() {
                    workspace.open_paths(&paths);
                    // focus window after open doc
                    floem::action::focus_window();
                }
            }
        });
        std::thread::Builder::new()
            .name("ListenLocalSocket".to_owned())
            .spawn(move || {
                if let Err(err) = ipc::listen_local_socket(tx) {
                    tracing::error!("{:?}", err);
                }
            })
            .unwrap();
    }

    {
        let app_data = app_data.clone();
        app_data.app_command.listen(move |command| {
            app_data.run_app_command(command);
        });
    }

    app.on_event(move |event| match event {
        floem::AppEvent::WillTerminate => {
            app_data.app_terminated.set(true);
            if let Err(err) = db.insert_app(app_data.clone()) {
                tracing::error!("{:?}", err);
            }
        }
        floem::AppEvent::Reopen {
            has_visible_windows,
        } => {
            if !has_visible_windows {
                app_data.new_window(None);
            }
        }
    })
    .run();
}
