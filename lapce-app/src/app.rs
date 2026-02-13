#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::{
    io::{BufReader, IsTerminal, Read, Write},
    ops::Range,
    path::PathBuf,
    process::Stdio,
    rc::Rc,
    sync::{
        Arc,
        atomic::AtomicU64,
        mpsc::{SyncSender, channel, sync_channel},
    },
};

use anyhow::{Result, anyhow};
use clap::Parser;
use floem::{
    IntoView, View,
    action::show_context_menu,
    event::{Event, EventListener, EventPropagation},
    ext_event::{create_ext_action, create_signal_from_channel},
    menu::{Menu, MenuItem},
    peniko::{
        Color,
        kurbo::{Point, Rect, Size},
    },
    prelude::SignalTrack,
    reactive::{
        ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        create_effect, create_memo, create_rw_signal, provide_context, use_context,
    },
    style::{
        AlignItems, CursorStyle, Display, FlexDirection, JustifyContent, Position,
        Style,
    },
    taffy::{
        Line,
        style_helpers::{self, auto, fr},
    },
    text::Weight,
    unit::PxPctAuto,
    views::{
        Decorators, VirtualVector, clip, container, drag_resize_window_area,
        drag_window_area, dyn_stack,
        editor::{core::register::Clipboard, text::SystemClipboard},
        empty, label, rich_text,
        scroll::{PropagatePointerWheel, VerticalScrollAsHorizontal, scroll},
        stack, svg, tab, text, tooltip, virtual_stack,
    },
    window::{ResizeDirection, WindowConfig, WindowId},
};
use include_dir::{Dir, include_dir};
use lapce_core::{
    command::FocusCommand,
    directory::Directory,
    meta,
    syntax::{Syntax, highlight::reset_highlight_configs},
};
use lapce_rpc::{
    RpcMessage,
    core::{CoreMessage, CoreNotification},
    file::PathObject,
};
use lsp_types::{CompletionItemKind, MessageType, ShowMessageParams};
use notify::Watcher;
use serde::{Deserialize, Serialize};
use tracing_subscriber::{filter::Targets, reload::Handle};

use crate::{
    about, alert,
    code_action::CodeActionStatus,
    command::{CommandKind, InternalCommand, LapceCommand, LapceWorkbenchCommand},
    config::{
        LapceConfig, color::LapceColor, icon::LapceIcons, ui::TabSeparatorHeight,
        watcher::ConfigWatcher,
    },
    db::LapceDb,
    editor::{
        location::{EditorLocation, EditorPosition},
        view::editor_container_view,
    },
    editor_tab::{EditorTabChild, EditorTabData},
    focus_text::focus_text,
    id::{EditorTabId, SplitId},
    keymap::keymap_view,
    listener::Listener,
    main_split::{
        SplitContent, SplitData, SplitDirection, SplitMoveDirection, TabCloseKind,
    },
    markdown::MarkdownContent,
    palette::{
        PaletteStatus,
        item::{PaletteItem, PaletteItemContent},
    },
    panel::{position::PanelContainerPosition, view::panel_container_view},
    plugin::{PluginData, plugin_info_view},
    recent_files,
    settings::{settings_view, theme_color_settings_view},
    status::status,
    text_input::TextInputBuilder,
    title::title,
    tracing::*,
    update::ReleaseInfo,
    window::{TabsInfo, WindowData, WindowInfo},
    workspace::{LapceWorkspace, LapceWorkspaceType},
    workspace_data::{Focus, WorkspaceData},
};

mod grammars;
mod logging;

const BUNDLED_PLUGINS_DIR: Dir =
    include_dir!("$CARGO_MANIFEST_DIR/../defaults/plugins");

fn install_bundled_plugins() {
    let plugins_dir = match Directory::plugins_directory() {
        Some(dir) => dir,
        None => return,
    };

    for entry in BUNDLED_PLUGINS_DIR.dirs() {
        let name = match entry.path().file_name() {
            Some(name) => name,
            None => continue,
        };
        let target = plugins_dir.join(name);
        if target.exists() {
            continue;
        }
        if let Err(err) = extract_dir(entry, &target) {
            tracing::error!(
                "Failed to install bundled plugin {:?}: {:?}",
                name,
                err
            );
        }
    }
}

fn extract_dir(dir: &Dir, target: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for file in dir.files() {
        let file_path = target.join(file.path().file_name().unwrap());
        std::fs::write(&file_path, file.contents())?;
    }
    for subdir in dir.dirs() {
        let subdir_name = subdir.path().file_name().unwrap();
        extract_dir(subdir, &target.join(subdir_name))?;
    }
    Ok(())
}

#[derive(Parser)]
#[clap(name = "Lapce")]
#[clap(version=meta::VERSION)]
#[derive(Debug)]
struct Cli {
    /// Launch new window even if Lapce is already running
    #[clap(short, long, action)]
    new: bool,
    /// Don't return instantly when opened in a terminal
    #[clap(short, long, action)]
    wait: bool,

    /// Path(s) to plugins to load.  
    /// This is primarily used for plugin development to make it easier to test changes to the
    /// plugin without needing to copy the plugin to the plugins directory.  
    /// This will cause any plugin with the same author & name to not run.
    #[clap(long, action)]
    plugin_path: Vec<PathBuf>,

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

#[derive(Clone)]
pub enum AppCommand {
    SaveApp,
    NewWindow { folder: Option<PathBuf> },
    CloseWindow(WindowId),
    WindowGotFocus(WindowId),
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
    /// Paths to extra plugins to load
    pub plugin_paths: Arc<Vec<PathBuf>>,
}

impl AppData {
    pub fn reload_config(&self) {
        let config =
            LapceConfig::load(&LapceWorkspace::default(), &[], &self.plugin_paths);

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

    fn default_window_config(&self) -> WindowConfig {
        WindowConfig::default()
            .apply_default_theme(false)
            .title("Lapce")
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
                let db: Arc<LapceDb> = use_context().unwrap();
                db.get_window().ok().map(|info| {
                    self.default_window_config()
                        .size(info.size)
                        .position(info.pos)
                })
            })
            .unwrap_or_else(|| {
                self.default_window_config().size(Size::new(800.0, 600.0))
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
                let db: Arc<LapceDb> = use_context().unwrap();
                if let Err(err) = db.save_app(self) {
                    tracing::error!("{:?}", err);
                }
            }
            AppCommand::WindowClosed(window_id) => {
                if self.app_terminated.get_untracked() {
                    return;
                }
                let db: Arc<LapceDb> = use_context().unwrap();
                if self.windows.with_untracked(|w| w.len()) == 1 {
                    if let Err(err) = db.insert_app(self.clone()) {
                        tracing::error!("{:?}", err);
                    }
                }
                let window_data = self
                    .windows
                    .try_update(|windows| windows.remove(&window_id))
                    .unwrap();
                if let Some(window_data) = window_data {
                    window_data.scope.dispose();
                }
                if let Err(err) = db.save_app(self) {
                    tracing::error!("{:?}", err);
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

    fn create_windows(
        &self,
        db: Arc<LapceDb>,
        paths: Vec<PathObject>,
    ) -> floem::Application {
        let mut app = floem::Application::new();

        let mut inital_windows = 0;

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
                .unwrap_or_else(|_| (Size::new(800.0, 600.0), Point::new(0.0, 0.0)));

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
                inital_windows += 1;
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
                        inital_windows += 1;
                    }
                }
                Err(err) => {
                    tracing::error!("{:?}", err);
                }
            }
        }

        if inital_windows == 0 {
            let mut info = db.get_window().unwrap_or_else(|_| WindowInfo {
                size: Size::new(800.0, 600.0),
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
            self.plugin_paths.clone(),
            self.app_command,
        );

        {
            let cur_workspace = window_data.active.get_untracked();
            let (_, workspace) =
                &window_data.workspaces.get_untracked()[cur_workspace];
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

/// The top bar of an Editor tab. Includes the tab forward/back buttons, the tab scroll bar and the new split and tab close all button.
fn editor_tab_header(
    workspace_data: Rc<WorkspaceData>,
    active_editor_tab: ReadSignal<Option<EditorTabId>>,
    editor_tab: RwSignal<EditorTabData>,
    dragging: RwSignal<Option<(RwSignal<usize>, EditorTabId)>>,
) -> impl View {
    let main_split = workspace_data.main_split.clone();
    let plugin = workspace_data.plugin.clone();
    let editors = workspace_data.main_split.editors;
    let focus = workspace_data.common.focus;
    let config = workspace_data.common.config;
    let internal_command = workspace_data.common.internal_command;
    let editor_tab_id =
        editor_tab.with_untracked(|editor_tab| editor_tab.editor_tab_id);

    let editor_tab_active =
        create_memo(move |_| editor_tab.with(|editor_tab| editor_tab.active));
    let items = move || {
        let editor_tab = editor_tab.get();
        for (i, (index, _, _)) in editor_tab.children.iter().enumerate() {
            if index.get_untracked() != i {
                index.set(i);
            }
        }
        editor_tab.children
    };
    let key = |(_, _, child): &(RwSignal<usize>, RwSignal<Rect>, EditorTabChild)| {
        child.id()
    };
    let is_focused = move || {
        if let Focus::Workbench = focus.get() {
            editor_tab.with_untracked(|e| Some(e.editor_tab_id))
                == active_editor_tab.get()
        } else {
            false
        }
    };

    let view_fn = move |(i, layout_rect, child): (
        RwSignal<usize>,
        RwSignal<Rect>,
        EditorTabChild,
    )| {
        let child_for_close = child.clone();
        let child_for_mouse_close = child.clone();
        let child_for_mouse_close_2 = child.clone();
        let main_split = main_split.clone();
        let plugin = plugin.clone();
        let child_view = {
            let info = child.view_info(editors, plugin, config);
            let hovered = create_rw_signal(false);

            use crate::config::ui::TabCloseButton;

            let tab_icon = container({
                svg("")
                    .update_value(move || info.with(|info| info.icon.clone()))
                    .style(move |s| {
                        let config = config.get();
                        let size = config.ui.icon_size() as f32;
                        s.size(size, size)
                            .apply_opt(info.with(|info| info.color), |s, c| {
                                s.color(c)
                            })
                            .apply_if(
                                !info.with(|info| info.is_pristine)
                                    && config.ui.tab_close_button
                                        == TabCloseButton::Off,
                                |s| s.color(config.color(LapceColor::LAPCE_WARN)),
                            )
                    })
            })
            .style(|s| s.padding(4.));

            let tab_content = tooltip(
                label(move || info.with(|info| info.name.clone()))
                    .style(move |s| s.selectable(false)),
                move || {
                    tooltip_tip(
                        config,
                        text(info.with(|info| {
                            info.path
                                .clone()
                                .map(|path| path.display().to_string())
                                .unwrap_or("local".to_string())
                        })),
                    )
                },
            );

            let tab_close_button = clickable_icon(
                move || {
                    if hovered.get() || info.with(|info| info.is_pristine) {
                        LapceIcons::CLOSE
                    } else {
                        LapceIcons::UNSAVED
                    }
                },
                move || {
                    let editor_tab_id =
                        editor_tab.with_untracked(|t| t.editor_tab_id);
                    internal_command.send(InternalCommand::EditorTabChildClose {
                        editor_tab_id,
                        child: child_for_close.clone(),
                    });
                },
                || false,
                || false,
                || "Close",
                config,
            )
            .on_event_stop(EventListener::PointerDown, |_| {})
            .on_event_stop(EventListener::PointerEnter, move |_| {
                hovered.set(true);
            })
            .on_event_stop(EventListener::PointerLeave, move |_| {
                hovered.set(false);
            });

            stack((
                tab_icon.style(move |s| {
                    let tab_close_button = config.get().ui.tab_close_button;
                    s.apply_if(tab_close_button == TabCloseButton::Left, |s| {
                        s.grid_column(Line {
                            start: style_helpers::line(3),
                            end: style_helpers::span(1),
                        })
                    })
                }),
                tab_content.style(move |s| {
                    let tab_close_button = config.get().ui.tab_close_button;
                    s.apply_if(tab_close_button == TabCloseButton::Left, |s| {
                        s.grid_column(Line {
                            start: style_helpers::line(2),
                            end: style_helpers::span(1),
                        })
                    })
                    .apply_if(tab_close_button == TabCloseButton::Off, |s| {
                        s.padding_right(4.)
                    })
                }),
                tab_close_button.style(move |s| {
                    let tab_close_button = config.get().ui.tab_close_button;
                    s.apply_if(tab_close_button == TabCloseButton::Left, |s| {
                        s.grid_column(Line {
                            start: style_helpers::line(1),
                            end: style_helpers::span(1),
                        })
                    })
                    .apply_if(tab_close_button == TabCloseButton::Off, |s| s.hide())
                }),
            ))
            .style(move |s| {
                s.items_center()
                    .justify_center()
                    .border_left(if i.get() == 0 { 1.0 } else { 0.0 })
                    .border_right(1.0)
                    .border_color(config.get().color(LapceColor::LAPCE_BORDER))
                    .padding_horiz(6.)
                    .gap(6.)
                    .grid()
                    .grid_template_columns(vec![auto(), fr(1.), auto()])
                    .apply_if(
                        config.get().ui.tab_separator_height
                            == TabSeparatorHeight::Full,
                        |s| s.height_full(),
                    )
            })
        };

        let header_content_size = create_rw_signal(Size::ZERO);
        let drag_over_left: RwSignal<Option<bool>> = create_rw_signal(None);
        stack((
            child_view
                .on_event(EventListener::PointerDown, move |event| {
                    if let Event::PointerDown(pointer_event) = event {
                        if pointer_event.button.is_auxiliary() {
                            let editor_tab_id =
                                editor_tab.with_untracked(|t| t.editor_tab_id);
                            internal_command.send(
                                InternalCommand::EditorTabChildClose {
                                    editor_tab_id,
                                    child: child_for_mouse_close.clone(),
                                },
                            );
                            EventPropagation::Stop
                        } else {
                            editor_tab.update(|editor_tab| {
                                editor_tab.active = i.get_untracked();
                            });
                            EventPropagation::Continue
                        }
                    } else {
                        EventPropagation::Continue
                    }
                })
                .on_secondary_click_stop(move |_| {
                    let editor_tab_id =
                        editor_tab.with_untracked(|t| t.editor_tab_id);

                    tab_secondary_click(
                        internal_command,
                        editor_tab_id,
                        child_for_mouse_close_2.clone(),
                    );
                })
                .on_event_stop(EventListener::DragStart, move |_| {
                    dragging.set(Some((i, editor_tab_id)));
                })
                .on_event_stop(EventListener::DragEnd, move |_| {
                    dragging.set(None);
                })
                .on_resize(move |rect| {
                    header_content_size.set(rect.size());
                })
                .draggable()
                .dragging_style(move |s| {
                    let config = config.get();
                    s.border(1.0)
                        .border_radius(6.0)
                        .background(
                            config
                                .color(LapceColor::PANEL_BACKGROUND)
                                .multiply_alpha(0.7),
                        )
                        .border_color(config.color(LapceColor::LAPCE_BORDER))
                })
                .style(|s| s.align_items(Some(AlignItems::Center)).flex_grow(1.0)),
            empty()
                .style(move |s| {
                    s.size_full()
                        .border_bottom(if editor_tab_active.get() == i.get() {
                            2.0
                        } else {
                            0.0
                        })
                        .border_color(config.get().color(if is_focused() {
                            LapceColor::LAPCE_TAB_ACTIVE_UNDERLINE
                        } else {
                            LapceColor::LAPCE_TAB_INACTIVE_UNDERLINE
                        }))
                })
                .style(|s| {
                    s.absolute()
                        .padding_horiz(3.0)
                        .size_full()
                        .pointer_events_none()
                })
                .debug_name("Drop Indicator"),
            empty()
                .style(move |s| {
                    let i = i.get();
                    let drag_over_left = drag_over_left.get();
                    s.absolute()
                        .margin_left(if i == 0 { 0.0 } else { -2.0 })
                        .height_full()
                        .width(
                            header_content_size.get().width as f32
                                + if i == 0 { 1.0 } else { 3.0 },
                        )
                        .apply_if(drag_over_left.is_none(), |s| s.hide())
                        .apply_if(drag_over_left.is_some(), |s| {
                            if let Some(drag_over_left) = drag_over_left {
                                if drag_over_left {
                                    s.border_left(3.0)
                                } else {
                                    s.border_right(3.0)
                                }
                            } else {
                                s
                            }
                        })
                        .border_color(
                            config
                                .get()
                                .color(LapceColor::LAPCE_TAB_ACTIVE_UNDERLINE)
                                .multiply_alpha(0.5),
                        )
                })
                .debug_name("Active Tab Indicator"),
        ))
        .on_resize(move |rect| {
            layout_rect.set(rect);
        })
        .style(move |s| {
            let config = config.get();
            s.height_full()
                .flex_col()
                .items_center()
                .justify_center()
                .cursor(CursorStyle::Pointer)
                .hover(|s| s.background(config.color(LapceColor::HOVER_BACKGROUND)))
        })
        .debug_name("Tab and Active Indicator")
        .on_event_stop(EventListener::DragOver, move |event| {
            if dragging.with_untracked(|dragging| dragging.is_some()) {
                if let Event::PointerMove(pointer_event) = event {
                    let new_left = pointer_event.pos.x
                        < header_content_size.get_untracked().width / 2.0;
                    if drag_over_left.get_untracked() != Some(new_left) {
                        drag_over_left.set(Some(new_left));
                    }
                }
            }
        })
        .on_event(EventListener::Drop, move |event| {
            if let Some((from_index, from_editor_tab_id)) = dragging.get_untracked()
            {
                drag_over_left.set(None);
                if let Event::PointerUp(pointer_event) = event {
                    let left = pointer_event.pos.x
                        < header_content_size.get_untracked().width / 2.0;
                    let index = i.get_untracked();
                    let new_index = if left { index } else { index + 1 };
                    main_split.move_editor_tab_child(
                        from_editor_tab_id,
                        editor_tab_id,
                        from_index.get_untracked(),
                        new_index,
                    );
                }
                EventPropagation::Stop
            } else {
                EventPropagation::Continue
            }
        })
        .on_event_stop(EventListener::DragLeave, move |_| {
            drag_over_left.set(None);
        })
    };

    let content_size = create_rw_signal(Size::ZERO);
    let scroll_offset = create_rw_signal(Rect::ZERO);
    stack((
        container(
            scroll({
                dyn_stack(items, key, view_fn)
                    .on_resize(move |rect| {
                        let size = rect.size();
                        if content_size.get_untracked() != size {
                            content_size.set(size);
                        }
                    })
                    .debug_name("Horizontal Tab Stack")
                    .style(|s| s.height_full().items_center())
            })
            .on_scroll(move |rect| {
                scroll_offset.set(rect);
            })
            .ensure_visible(move || {
                let active = editor_tab_active.get();
                editor_tab
                    .with_untracked(|editor_tab| editor_tab.children[active].1)
                    .get_untracked()
            })
            .scroll_style(|s| s.hide_bars(true))
            .style(|s| {
                s.set(VerticalScrollAsHorizontal, true)
                    .absolute()
                    .size_full()
            }),
        )
        .style(|s| s.height_full().flex_grow(1.0).flex_basis(0.).min_width(10.))
        .debug_name("Tab scroll"),
        stack({
            let size = create_rw_signal(Size::ZERO);
            (
                clip({
                    empty().style(move |s| {
                        let config = config.get();
                        s.absolute()
                            .height_full()
                            .margin_left(30.0)
                            .width(size.get().width as f32)
                            .background(config.color(LapceColor::PANEL_BACKGROUND))
                            .box_shadow_blur(3.0)
                            .box_shadow_color(
                                config.color(LapceColor::LAPCE_DROPDOWN_SHADOW),
                            )
                    })
                })
                .style(move |s| {
                    let content_size = content_size.get();
                    let scroll_offset = scroll_offset.get();
                    s.absolute()
                        .margin_left(-30.0)
                        .width(size.get().width as f32 + 30.0)
                        .height_full()
                        .apply_if(scroll_offset.x1 >= content_size.width, |s| {
                            s.hide()
                        })
                }),
                stack((
                    clickable_icon(
                        || LapceIcons::SPLIT_HORIZONTAL,
                        move || {
                            let editor_tab_id =
                                editor_tab.with_untracked(|t| t.editor_tab_id);
                            internal_command.send(InternalCommand::Split {
                                direction: SplitDirection::Vertical,
                                editor_tab_id,
                            });
                        },
                        || false,
                        || false,
                        || "Split Horizontally",
                        config,
                    )
                    .style(|s| s.margin_left(6.0)),
                    clickable_icon(
                        || LapceIcons::CLOSE,
                        move || {
                            let editor_tab_id =
                                editor_tab.with_untracked(|t| t.editor_tab_id);
                            internal_command.send(InternalCommand::EditorTabClose {
                                editor_tab_id,
                            });
                        },
                        || false,
                        || false,
                        || "Close All",
                        config,
                    )
                    .style(|s| s.margin_horiz(6.0)),
                ))
                .on_resize(move |rect| {
                    size.set(rect.size());
                })
                .style(|s| s.items_center().height_full()),
            )
        })
        .debug_name("Split/Close Panel Buttons")
        .style(move |s| {
            let content_size = content_size.get();
            let scroll_offset = scroll_offset.get();
            s.height_full()
                .flex_shrink(0.)
                .margin_left(PxPctAuto::Auto)
                .apply_if(scroll_offset.x1 < content_size.width, |s| {
                    s.margin_left(0.)
                })
        }),
    ))
    .style(move |s| {
        let config = config.get();
        s.items_center()
            .max_width_full()
            .border_bottom(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::PANEL_BACKGROUND))
            .height(config.ui.header_height() as i32)
    })
    .debug_name("Editor Tab Header")
}

fn editor_tab_content(
    workspace_data: Rc<WorkspaceData>,
    plugin: PluginData,
    active_editor_tab: ReadSignal<Option<EditorTabId>>,
    editor_tab: RwSignal<EditorTabData>,
) -> impl View {
    let main_split = workspace_data.main_split.clone();
    let common = main_split.common.clone();
    let workspace = common.workspace.clone();
    let editors = main_split.editors;
    let focus = common.focus;
    let items = move || {
        editor_tab
            .get()
            .children
            .into_iter()
            .map(|(_, _, child)| child)
    };
    let key = |child: &EditorTabChild| child.id();
    let view_fn = move |child| {
        let common = common.clone();
        let child = match child {
            EditorTabChild::Editor(editor_id) => {
                if let Some(editor_data) = editors.editor_untracked(editor_id) {
                    let editor_scope = editor_data.scope;
                    let editor_tab_id = editor_data.editor_tab_id;
                    let is_active = move |tracked: bool| {
                        editor_scope.track();
                        let focus = if tracked {
                            focus.get()
                        } else {
                            focus.get_untracked()
                        };
                        if let Focus::Workbench = focus {
                            let active_editor_tab = if tracked {
                                active_editor_tab.get()
                            } else {
                                active_editor_tab.get_untracked()
                            };
                            let editor_tab = if tracked {
                                editor_tab_id.get()
                            } else {
                                editor_tab_id.get_untracked()
                            };
                            editor_tab.is_some() && editor_tab == active_editor_tab
                        } else {
                            false
                        }
                    };
                    let editor_data = create_rw_signal(editor_data);
                    editor_container_view(
                        workspace_data.clone(),
                        workspace.clone(),
                        is_active,
                        editor_data,
                    )
                    .into_any()
                } else {
                    text("empty editor").into_any()
                }
            }
            EditorTabChild::Settings(_) => {
                settings_view(plugin.installed, editors, common).into_any()
            }
            EditorTabChild::ThemeColorSettings(_) => {
                theme_color_settings_view(editors, common).into_any()
            }
            EditorTabChild::Keymap(_) => keymap_view(editors, common).into_any(),
            EditorTabChild::Volt(_, id) => {
                plugin_info_view(plugin.clone(), id).into_any()
            }
        };
        child.style(|s| s.size_full())
    };
    let active = move || editor_tab.with(|t| t.active);

    tab(active, items, key, view_fn)
        .style(|s| s.size_full())
        .debug_name("Editor Tab Content")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragOverPosition {
    Top,
    Bottom,
    Left,
    Right,
    Middle,
}

fn editor_tab(
    workspace_data: Rc<WorkspaceData>,
    plugin: PluginData,
    active_editor_tab: ReadSignal<Option<EditorTabId>>,
    editor_tab: RwSignal<EditorTabData>,
    dragging: RwSignal<Option<(RwSignal<usize>, EditorTabId)>>,
) -> impl View {
    let main_split = workspace_data.main_split.clone();
    let common = main_split.common.clone();
    let editor_tabs = main_split.editor_tabs;
    let editor_tab_id =
        editor_tab.with_untracked(|editor_tab| editor_tab.editor_tab_id);
    let config = common.config;
    let focus = common.focus;
    let internal_command = main_split.common.internal_command;
    let tab_size = create_rw_signal(Size::ZERO);
    let drag_over: RwSignal<Option<DragOverPosition>> = create_rw_signal(None);
    stack((
        editor_tab_header(
            workspace_data.clone(),
            active_editor_tab,
            editor_tab,
            dragging,
        ),
        stack((
            editor_tab_content(
                workspace_data.clone(),
                plugin.clone(),
                active_editor_tab,
                editor_tab,
            ),
            empty()
                .style(move |s| {
                    let pos = drag_over.get();
                    let width = match pos {
                        Some(pos) => match pos {
                            DragOverPosition::Top => 100.0,
                            DragOverPosition::Bottom => 100.0,
                            DragOverPosition::Left => 50.0,
                            DragOverPosition::Right => 50.0,
                            DragOverPosition::Middle => 100.0,
                        },
                        None => 100.0,
                    };
                    let height = match pos {
                        Some(pos) => match pos {
                            DragOverPosition::Top => 50.0,
                            DragOverPosition::Bottom => 50.0,
                            DragOverPosition::Left => 100.0,
                            DragOverPosition::Right => 100.0,
                            DragOverPosition::Middle => 100.0,
                        },
                        None => 100.0,
                    };
                    let size = tab_size.get_untracked();
                    let margin_left = match pos {
                        Some(pos) => match pos {
                            DragOverPosition::Top => 0.0,
                            DragOverPosition::Bottom => 0.0,
                            DragOverPosition::Left => 0.0,
                            DragOverPosition::Right => size.width / 2.0,
                            DragOverPosition::Middle => 0.0,
                        },
                        None => 0.0,
                    };
                    let margin_top = match pos {
                        Some(pos) => match pos {
                            DragOverPosition::Top => 0.0,
                            DragOverPosition::Bottom => size.height / 2.0,
                            DragOverPosition::Left => 0.0,
                            DragOverPosition::Right => 0.0,
                            DragOverPosition::Middle => 0.0,
                        },
                        None => 0.0,
                    };
                    s.absolute()
                        .size_pct(width, height)
                        .margin_top(margin_top as f32)
                        .margin_left(margin_left as f32)
                        .apply_if(pos.is_none(), |s| s.hide())
                        .background(
                            config
                                .get()
                                .color(LapceColor::EDITOR_DRAG_DROP_BACKGROUND),
                        )
                })
                .debug_name("Drag Over Handle"),
            empty()
                .on_event_stop(EventListener::DragOver, move |event| {
                    if dragging.with_untracked(|dragging| dragging.is_some()) {
                        if let Event::PointerMove(pointer_event) = event {
                            let size = tab_size.get_untracked();
                            let pos = pointer_event.pos;
                            let new_drag_over = if pos.x < size.width / 4.0 {
                                DragOverPosition::Left
                            } else if pos.x > size.width * 3.0 / 4.0 {
                                DragOverPosition::Right
                            } else if pos.y < size.height / 4.0 {
                                DragOverPosition::Top
                            } else if pos.y > size.height * 3.0 / 4.0 {
                                DragOverPosition::Bottom
                            } else {
                                DragOverPosition::Middle
                            };
                            if drag_over.get_untracked() != Some(new_drag_over) {
                                drag_over.set(Some(new_drag_over));
                            }
                        }
                    }
                })
                .on_event_stop(EventListener::DragLeave, move |_| {
                    drag_over.set(None);
                })
                .on_event(EventListener::Drop, move |_| {
                    if let Some((from_index, from_editor_tab_id)) =
                        dragging.get_untracked()
                    {
                        if let Some(pos) = drag_over.get_untracked() {
                            match pos {
                                DragOverPosition::Top => {
                                    main_split.move_editor_tab_child_to_new_split(
                                        from_editor_tab_id,
                                        from_index.get_untracked(),
                                        editor_tab_id,
                                        SplitMoveDirection::Up,
                                    );
                                }
                                DragOverPosition::Bottom => {
                                    main_split.move_editor_tab_child_to_new_split(
                                        from_editor_tab_id,
                                        from_index.get_untracked(),
                                        editor_tab_id,
                                        SplitMoveDirection::Down,
                                    );
                                }
                                DragOverPosition::Left => {
                                    main_split.move_editor_tab_child_to_new_split(
                                        from_editor_tab_id,
                                        from_index.get_untracked(),
                                        editor_tab_id,
                                        SplitMoveDirection::Left,
                                    );
                                }
                                DragOverPosition::Right => {
                                    main_split.move_editor_tab_child_to_new_split(
                                        from_editor_tab_id,
                                        from_index.get_untracked(),
                                        editor_tab_id,
                                        SplitMoveDirection::Right,
                                    );
                                }
                                DragOverPosition::Middle => {
                                    main_split.move_editor_tab_child(
                                        from_editor_tab_id,
                                        editor_tab_id,
                                        from_index.get_untracked(),
                                        editor_tab.with_untracked(|editor_tab| {
                                            editor_tab.active + 1
                                        }),
                                    );
                                }
                            }
                        }
                        drag_over.set(None);
                        EventPropagation::Stop
                    } else {
                        EventPropagation::Continue
                    }
                })
                .on_resize(move |rect| {
                    tab_size.set(rect.size());
                })
                .style(move |s| {
                    s.absolute()
                        .size_full()
                        .apply_if(dragging.get().is_none(), |s| {
                            s.pointer_events_none()
                        })
                }),
        ))
        .debug_name("Editor Content and Drag Over")
        .style(|s| s.size_full()),
    ))
    .on_event_cont(EventListener::PointerDown, move |_| {
        if focus.get_untracked() != Focus::Workbench {
            focus.set(Focus::Workbench);
        }
        let editor_tab_id = editor_tab.with_untracked(|t| t.editor_tab_id);
        internal_command.send(InternalCommand::FocusEditorTab { editor_tab_id });
    })
    .on_cleanup(move || {
        if editor_tabs
            .with_untracked(|editor_tabs| editor_tabs.contains_key(&editor_tab_id))
        {
            return;
        }
        editor_tab
            .with_untracked(|editor_tab| editor_tab.scope)
            .dispose();
    })
    .style(|s| s.flex_col().size_full())
    .debug_name("Editor Tab (Content + Header)")
}

fn split_resize_border(
    splits: ReadSignal<im::HashMap<SplitId, RwSignal<SplitData>>>,
    editor_tabs: ReadSignal<im::HashMap<EditorTabId, RwSignal<EditorTabData>>>,
    split: ReadSignal<SplitData>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let content_rect = move |content: &SplitContent, tracked: bool| {
        if tracked {
            match content {
                SplitContent::EditorTab(editor_tab_id) => {
                    let editor_tab_data =
                        editor_tabs.with(|tabs| tabs.get(editor_tab_id).cloned());
                    if let Some(editor_tab_data) = editor_tab_data {
                        editor_tab_data.with(|editor_tab| editor_tab.layout_rect)
                    } else {
                        Rect::ZERO
                    }
                }
                SplitContent::Split(split_id) => {
                    if let Some(split) =
                        splits.with(|splits| splits.get(split_id).cloned())
                    {
                        split.with(|split| split.layout_rect)
                    } else {
                        Rect::ZERO
                    }
                }
            }
        } else {
            match content {
                SplitContent::EditorTab(editor_tab_id) => {
                    let editor_tab_data = editor_tabs
                        .with_untracked(|tabs| tabs.get(editor_tab_id).cloned());
                    if let Some(editor_tab_data) = editor_tab_data {
                        editor_tab_data
                            .with_untracked(|editor_tab| editor_tab.layout_rect)
                    } else {
                        Rect::ZERO
                    }
                }
                SplitContent::Split(split_id) => {
                    if let Some(split) =
                        splits.with_untracked(|splits| splits.get(split_id).cloned())
                    {
                        split.with_untracked(|split| split.layout_rect)
                    } else {
                        Rect::ZERO
                    }
                }
            }
        }
    };
    let direction = move |tracked: bool| {
        if tracked {
            split.with(|split| split.direction)
        } else {
            split.with_untracked(|split| split.direction)
        }
    };
    dyn_stack(
        move || {
            let data = split.get();
            data.children.into_iter().enumerate().skip(1)
        },
        |(index, (_, content))| (*index, content.id()),
        move |(index, (_, content))| {
            let drag_start: RwSignal<Option<Point>> = create_rw_signal(None);
            let view = empty();
            let view_id = view.id();
            view.on_event_stop(EventListener::PointerDown, move |event| {
                view_id.request_active();
                if let Event::PointerDown(pointer_event) = event {
                    drag_start.set(Some(pointer_event.pos));
                }
            })
            .on_event_stop(EventListener::PointerUp, move |_| {
                drag_start.set(None);
            })
            .on_event_stop(EventListener::PointerMove, move |event| {
                if let Event::PointerMove(pointer_event) = event {
                    if let Some(drag_start_point) = drag_start.get_untracked() {
                        let rects = split.with_untracked(|split| {
                            split
                                .children
                                .iter()
                                .map(|(_, c)| content_rect(c, false))
                                .collect::<Vec<Rect>>()
                        });
                        let direction = direction(false);
                        match direction {
                            SplitDirection::Vertical => {
                                let left = rects[index - 1].width();
                                let right = rects[index].width();
                                let shift = pointer_event.pos.x - drag_start_point.x;
                                let left = left + shift;
                                let right = right - shift;
                                let total_width =
                                    rects.iter().map(|r| r.width()).sum::<f64>();
                                split.with_untracked(|split| {
                                    for (i, (size, _)) in
                                        split.children.iter().enumerate()
                                    {
                                        if i == index - 1 {
                                            size.set(left / total_width);
                                        } else if i == index {
                                            size.set(right / total_width);
                                        } else {
                                            size.set(rects[i].width() / total_width);
                                        }
                                    }
                                })
                            }
                            SplitDirection::Horizontal => {
                                let up = rects[index - 1].height();
                                let down = rects[index].height();
                                let shift = pointer_event.pos.y - drag_start_point.y;
                                let up = up + shift;
                                let down = down - shift;
                                let total_height =
                                    rects.iter().map(|r| r.height()).sum::<f64>();
                                split.with_untracked(|split| {
                                    for (i, (size, _)) in
                                        split.children.iter().enumerate()
                                    {
                                        if i == index - 1 {
                                            size.set(up / total_height);
                                        } else if i == index {
                                            size.set(down / total_height);
                                        } else {
                                            size.set(
                                                rects[i].height() / total_height,
                                            );
                                        }
                                    }
                                })
                            }
                        }
                    }
                }
            })
            .style(move |s| {
                let rect = content_rect(&content, true);
                let is_dragging = drag_start.get().is_some();
                let direction = direction(true);
                s.position(Position::Absolute)
                    .apply_if(direction == SplitDirection::Vertical, |style| {
                        style.margin_left(rect.x0 as f32 - 0.0)
                    })
                    .apply_if(direction == SplitDirection::Horizontal, |style| {
                        style.margin_top(rect.y0 as f32 - 0.0)
                    })
                    .width(match direction {
                        SplitDirection::Vertical => PxPctAuto::Px(4.0),
                        SplitDirection::Horizontal => PxPctAuto::Pct(100.0),
                    })
                    .height(match direction {
                        SplitDirection::Vertical => PxPctAuto::Pct(100.0),
                        SplitDirection::Horizontal => PxPctAuto::Px(4.0),
                    })
                    .flex_direction(match direction {
                        SplitDirection::Vertical => FlexDirection::Row,
                        SplitDirection::Horizontal => FlexDirection::Column,
                    })
                    .apply_if(is_dragging, |s| {
                        s.cursor(match direction {
                            SplitDirection::Vertical => CursorStyle::ColResize,
                            SplitDirection::Horizontal => CursorStyle::RowResize,
                        })
                        .background(config.get().color(LapceColor::EDITOR_CARET))
                    })
                    .hover(|s| {
                        s.cursor(match direction {
                            SplitDirection::Vertical => CursorStyle::ColResize,
                            SplitDirection::Horizontal => CursorStyle::RowResize,
                        })
                        .background(config.get().color(LapceColor::EDITOR_CARET))
                    })
                    .pointer_events_auto()
            })
        },
    )
    .style(|s| {
        s.position(Position::Absolute)
            .size_full()
            .pointer_events_none()
    })
    .debug_name("Split Resize Border")
}

fn split_border(
    splits: ReadSignal<im::HashMap<SplitId, RwSignal<SplitData>>>,
    editor_tabs: ReadSignal<im::HashMap<EditorTabId, RwSignal<EditorTabData>>>,
    split: ReadSignal<SplitData>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let direction = move || split.with(|split| split.direction);
    dyn_stack(
        move || split.get().children.into_iter().skip(1),
        |(_, content)| content.id(),
        move |(_, content)| {
            container(empty().style(move |s| {
                let direction = direction();
                s.width(match direction {
                    SplitDirection::Vertical => PxPctAuto::Px(1.0),
                    SplitDirection::Horizontal => PxPctAuto::Pct(100.0),
                })
                .height(match direction {
                    SplitDirection::Vertical => PxPctAuto::Pct(100.0),
                    SplitDirection::Horizontal => PxPctAuto::Px(1.0),
                })
                .background(config.get().color(LapceColor::LAPCE_BORDER))
            }))
            .style(move |s| {
                let rect = match &content {
                    SplitContent::EditorTab(editor_tab_id) => {
                        let editor_tab_data = editor_tabs
                            .with(|tabs| tabs.get(editor_tab_id).cloned());
                        if let Some(editor_tab_data) = editor_tab_data {
                            editor_tab_data.with(|editor_tab| editor_tab.layout_rect)
                        } else {
                            Rect::ZERO
                        }
                    }
                    SplitContent::Split(split_id) => {
                        if let Some(split) =
                            splits.with(|splits| splits.get(split_id).cloned())
                        {
                            split.with(|split| split.layout_rect)
                        } else {
                            Rect::ZERO
                        }
                    }
                };
                let direction = direction();
                s.position(Position::Absolute)
                    .apply_if(direction == SplitDirection::Vertical, |style| {
                        style.margin_left(rect.x0 as f32 - 2.0)
                    })
                    .apply_if(direction == SplitDirection::Horizontal, |style| {
                        style.margin_top(rect.y0 as f32 - 2.0)
                    })
                    .width(match direction {
                        SplitDirection::Vertical => PxPctAuto::Px(4.0),
                        SplitDirection::Horizontal => PxPctAuto::Pct(100.0),
                    })
                    .height(match direction {
                        SplitDirection::Vertical => PxPctAuto::Pct(100.0),
                        SplitDirection::Horizontal => PxPctAuto::Px(4.0),
                    })
                    .flex_direction(match direction {
                        SplitDirection::Vertical => FlexDirection::Row,
                        SplitDirection::Horizontal => FlexDirection::Column,
                    })
                    .justify_content(Some(JustifyContent::Center))
            })
        },
    )
    .style(|s| {
        s.position(Position::Absolute)
            .size_full()
            .pointer_events_none()
    })
    .debug_name("Split Border")
}

fn split_list(
    split: ReadSignal<SplitData>,
    workspace_data: Rc<WorkspaceData>,
    plugin: PluginData,
    dragging: RwSignal<Option<(RwSignal<usize>, EditorTabId)>>,
) -> impl View {
    let main_split = workspace_data.main_split.clone();
    let editor_tabs = main_split.editor_tabs.read_only();
    let active_editor_tab = main_split.active_editor_tab.read_only();
    let splits = main_split.splits.read_only();
    let config = main_split.common.config;
    let split_id = split.with_untracked(|split| split.split_id);

    let direction = move || split.with(|split| split.direction);
    let items = move || split.get().children.into_iter().enumerate();
    let key = |(_index, (_, content)): &(usize, (RwSignal<f64>, SplitContent))| {
        content.id()
    };
    let view_fn = {
        let main_split = main_split.clone();
        let workspace_data = workspace_data.clone();
        move |(_index, (split_size, content)): (
            usize,
            (RwSignal<f64>, SplitContent),
        )| {
            let plugin = plugin.clone();
            let child = match &content {
                SplitContent::EditorTab(editor_tab_id) => {
                    let editor_tab_data = editor_tabs
                        .with_untracked(|tabs| tabs.get(editor_tab_id).cloned());
                    if let Some(editor_tab_data) = editor_tab_data {
                        editor_tab(
                            workspace_data.clone(),
                            plugin.clone(),
                            active_editor_tab,
                            editor_tab_data,
                            dragging,
                        )
                        .into_any()
                    } else {
                        text("empty editor tab").into_any()
                    }
                }
                SplitContent::Split(split_id) => {
                    if let Some(split) =
                        splits.with(|splits| splits.get(split_id).cloned())
                    {
                        split_list(
                            split.read_only(),
                            workspace_data.clone(),
                            plugin.clone(),
                            dragging,
                        )
                        .into_any()
                    } else {
                        text("empty split").into_any()
                    }
                }
            };
            let local_main_split = main_split.clone();
            let local_local_main_split = main_split.clone();
            child
                .on_resize(move |rect| match &content {
                    SplitContent::EditorTab(editor_tab_id) => {
                        local_main_split.editor_tab_update_layout(
                            editor_tab_id,
                            None,
                            Some(rect),
                        );
                    }
                    SplitContent::Split(split_id) => {
                        let split_data =
                            splits.with(|splits| splits.get(split_id).cloned());
                        if let Some(split_data) = split_data {
                            split_data.update(|split| {
                                split.layout_rect = rect;
                            });
                        }
                    }
                })
                .on_move(move |point| match &content {
                    SplitContent::EditorTab(editor_tab_id) => {
                        local_local_main_split.editor_tab_update_layout(
                            editor_tab_id,
                            Some(point),
                            None,
                        );
                    }
                    SplitContent::Split(split_id) => {
                        let split_data =
                            splits.with(|splits| splits.get(split_id).cloned());
                        if let Some(split_data) = split_data {
                            split_data.update(|split| {
                                split.window_origin = point;
                            });
                        }
                    }
                })
                .style(move |s| s.flex_grow(split_size.get() as f32).flex_basis(0.0))
        }
    };
    container(
        stack((
            dyn_stack(items, key, view_fn).style(move |s| {
                s.flex_direction(match direction() {
                    SplitDirection::Vertical => FlexDirection::Row,
                    SplitDirection::Horizontal => FlexDirection::Column,
                })
                .size_full()
            }),
            split_border(splits, editor_tabs, split, config),
            split_resize_border(splits, editor_tabs, split, config),
        ))
        .style(|s| s.size_full()),
    )
    .on_cleanup(move || {
        if splits.with_untracked(|splits| splits.contains_key(&split_id)) {
            return;
        }
        split
            .with_untracked(|split_data| split_data.scope)
            .dispose();
    })
    .debug_name("Split List")
}

fn main_split(workspace_data: Rc<WorkspaceData>) -> impl View {
    let root_split = workspace_data.main_split.root_split;
    let root_split = workspace_data
        .main_split
        .splits
        .get_untracked()
        .get(&root_split)
        .unwrap()
        .read_only();
    let config = workspace_data.main_split.common.config;
    let panel = workspace_data.panel.clone();
    let plugin = workspace_data.plugin.clone();
    let dragging: RwSignal<Option<(RwSignal<usize>, EditorTabId)>> =
        create_rw_signal(None);
    split_list(root_split, workspace_data.clone(), plugin.clone(), dragging)
        .style(move |s| {
            let config = config.get();
            let is_hidden = panel.panel_bottom_maximized(true)
                && panel.is_container_shown(&PanelContainerPosition::Bottom, true);
            s.border_color(config.color(LapceColor::LAPCE_BORDER))
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
                .apply_if(is_hidden, |s| s.display(Display::None))
                .width_full()
                .flex_grow(1.0)
                .flex_basis(0.0)
        })
        .debug_name("Main Split")
}

pub fn not_clickable_icon<S: std::fmt::Display + 'static>(
    icon: impl Fn() -> &'static str + 'static,
    active_fn: impl Fn() -> bool + 'static,
    disabled_fn: impl Fn() -> bool + 'static + Copy,
    tooltip_: impl Fn() -> S + 'static + Clone,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    tooltip_label(
        config,
        clickable_icon_base(
            icon,
            None::<Box<dyn Fn()>>,
            active_fn,
            disabled_fn,
            config,
        ),
        tooltip_,
    )
    .debug_name("Not Clickable Icon")
}

pub fn clickable_icon<S: std::fmt::Display + 'static>(
    icon: impl Fn() -> &'static str + 'static,
    on_click: impl Fn() + 'static,
    active_fn: impl Fn() -> bool + 'static,
    disabled_fn: impl Fn() -> bool + 'static + Copy,
    tooltip_: impl Fn() -> S + 'static + Clone,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    tooltip_label(
        config,
        clickable_icon_base(icon, Some(on_click), active_fn, disabled_fn, config),
        tooltip_,
    )
}

pub fn clickable_icon_base(
    icon: impl Fn() -> &'static str + 'static,
    on_click: Option<impl Fn() + 'static>,
    active_fn: impl Fn() -> bool + 'static,
    disabled_fn: impl Fn() -> bool + 'static + Copy,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let view = container(
        svg(move || config.get().ui_svg(icon()))
            .style(move |s| {
                let config = config.get();
                let size = config.ui.icon_size() as f32;
                s.size(size, size)
                    .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    .disabled(|s| {
                        s.color(config.color(LapceColor::LAPCE_ICON_INACTIVE))
                            .cursor(CursorStyle::Default)
                    })
            })
            .disabled(disabled_fn),
    )
    .disabled(disabled_fn)
    .style(move |s| {
        let config = config.get();
        s.padding(4.0)
            .border_radius(6.0)
            .border(1.0)
            .border_color(Color::TRANSPARENT)
            .apply_if(active_fn(), |s| {
                s.border_color(config.color(LapceColor::EDITOR_CARET))
            })
            .hover(|s| {
                s.cursor(CursorStyle::Pointer)
                    .background(config.color(LapceColor::PANEL_HOVERED_BACKGROUND))
            })
            .active(|s| {
                s.background(
                    config.color(LapceColor::PANEL_HOVERED_ACTIVE_BACKGROUND),
                )
            })
    });

    if let Some(on_click) = on_click {
        view.on_click_stop(move |_| {
            on_click();
        })
    } else {
        view
    }
}

/// A tooltip with a label inside.  
/// When styling an element that has the tooltip, it will style the child rather than the tooltip
/// label.
pub fn tooltip_label<S: std::fmt::Display + 'static, V: View + 'static>(
    config: ReadSignal<Arc<LapceConfig>>,
    child: V,
    text: impl Fn() -> S + 'static + Clone,
) -> impl View {
    tooltip(child, move || {
        tooltip_tip(
            config,
            label(text.clone()).style(move |s| s.selectable(false)),
        )
    })
}

fn tooltip_tip<V: View + 'static>(
    config: ReadSignal<Arc<LapceConfig>>,
    child: V,
) -> impl IntoView {
    container(child).style(move |s| {
        let config = config.get();
        s.padding_horiz(10.0)
            .padding_vert(5.0)
            .font_size(config.ui.font_size() as f32)
            .font_family(config.ui.font_family.clone())
            .color(config.color(LapceColor::TOOLTIP_FOREGROUND))
            .background(config.color(LapceColor::TOOLTIP_BACKGROUND))
            .border(1)
            .border_radius(6)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .box_shadow_blur(3.0)
            .box_shadow_color(config.color(LapceColor::LAPCE_DROPDOWN_SHADOW))
            .margin_left(0.0)
            .margin_top(4.0)
    })
}

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
                main_split(workspace_data.clone()),
                panel_container_view(workspace_data, PanelContainerPosition::Right),
            ))
            .on_resize(move |rect| {
                let width = rect.size().width;
                if main_split_width.get_untracked() != width {
                    main_split_width.set(width);
                }
            })
            .style(|s| s.flex_grow(1.0))
        },
        panel_container_view(workspace_data.clone(), PanelContainerPosition::Bottom),
        window_message_view(workspace_data.messages, workspace_data.common.config),
    ))
    .on_resize(move |rect| {
        let size = rect.size();
        if size != workbench_size.get_untracked() {
            workbench_size.set(size);
        }
    })
    .style(move |s| s.flex_col().size_full())
    .debug_name("Workbench")
}

fn empty_workspace_view(workspace_data: Rc<WorkspaceData>) -> impl View {
    let config = workspace_data.common.config;
    let workbench_command = workspace_data.common.workbench_command;

    drag_window_area(
        container(
            label(|| "Open Folder".to_string())
                .on_event_stop(EventListener::PointerDown, |_| {})
                .on_click_stop(move |_| {
                    workbench_command.send(LapceWorkbenchCommand::OpenFolder);
                })
                .style(move |s| {
                    let config = config.get();
                    s.padding_horiz(20.0)
                        .padding_vert(10.0)
                        .border_radius(6.0)
                        .color(
                            config
                                .color(LapceColor::LAPCE_BUTTON_PRIMARY_FOREGROUND),
                        )
                        .background(
                            config
                                .color(LapceColor::LAPCE_BUTTON_PRIMARY_BACKGROUND),
                        )
                        .font_size((config.ui.font_size() + 2) as f32)
                        .hover(|s| {
                            s.cursor(CursorStyle::Pointer).background(
                                config
                                    .color(
                                        LapceColor::LAPCE_BUTTON_PRIMARY_BACKGROUND,
                                    )
                                    .multiply_alpha(0.8),
                            )
                        })
                        .active(|s| {
                            s.background(
                                config
                                    .color(
                                        LapceColor::LAPCE_BUTTON_PRIMARY_BACKGROUND,
                                    )
                                    .multiply_alpha(0.6),
                            )
                        })
                }),
        )
        .style(|s| s.size_full().flex_col().items_center().justify_center()),
    )
}

fn palette_item(
    i: usize,
    item: PaletteItem,
    index: ReadSignal<usize>,
    palette_item_height: f64,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View + use<> {
    match &item.content {
        PaletteItemContent::File { path, .. }
        | PaletteItemContent::Reference { path, .. } => {
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            // let (file_name, _) = create_signal(cx.scope, file_name);
            let folder = path
                .parent()
                .unwrap_or("".as_ref())
                .to_string_lossy()
                .into_owned();
            // let (folder, _) = create_signal(cx.scope, folder);
            let folder_len = folder.len();

            let file_name_indices = item
                .indices
                .iter()
                .filter_map(|&i| {
                    if folder_len > 0 {
                        if i > folder_len {
                            Some(i - folder_len - 1)
                        } else {
                            None
                        }
                    } else {
                        Some(i)
                    }
                })
                .collect::<Vec<_>>();
            let folder_indices = item
                .indices
                .iter()
                .filter_map(|&i| if i < folder_len { Some(i) } else { None })
                .collect::<Vec<_>>();

            let path = path.to_path_buf();
            let style_path = path.clone();
            container(
                stack((
                    svg(move || config.get().file_svg(&path).0).style(move |s| {
                        let config = config.get();
                        let size = config.ui.icon_size() as f32;
                        let color = config.file_svg(&style_path).1;
                        s.min_width(size)
                            .size(size, size)
                            .margin_right(5.0)
                            .apply_opt(color, Style::color)
                    }),
                    focus_text(
                        move || file_name.clone(),
                        move || file_name_indices.clone(),
                        move || config.get().color(LapceColor::EDITOR_FOCUS),
                    )
                    .style(|s| s.margin_right(6.0).max_width_full()),
                    focus_text(
                        move || folder.clone(),
                        move || folder_indices.clone(),
                        move || config.get().color(LapceColor::EDITOR_FOCUS),
                    )
                    .style(move |s| {
                        s.color(config.get().color(LapceColor::EDITOR_DIM))
                            .min_width(0.0)
                            .flex_grow(1.0)
                            .flex_basis(0.0)
                    }),
                ))
                .style(|s| s.align_items(Some(AlignItems::Center)).max_width_full()),
            )
        }
        PaletteItemContent::Line { .. }
        | PaletteItemContent::Workspace { .. }
        | PaletteItemContent::Language { .. }
        | PaletteItemContent::LineEnding { .. }
        | PaletteItemContent::ColorTheme { .. }
        | PaletteItemContent::IconTheme { .. } => {
            let text = item.filter_text;
            let indices = item.indices;
            container(
                focus_text(
                    move || text.clone(),
                    move || indices.clone(),
                    move || config.get().color(LapceColor::EDITOR_FOCUS),
                )
                .style(|s| s.align_items(Some(AlignItems::Center)).max_width_full()),
            )
        }
    }
    .style(move |s| {
        s.width_full()
            .height(palette_item_height as f32)
            .padding_horiz(10.0)
            .apply_if(index.get() == i, |style| {
                style.background(
                    config.get().color(LapceColor::PALETTE_CURRENT_BACKGROUND),
                )
            })
    })
}

fn palette_input(workspace_data: Rc<WorkspaceData>) -> impl View {
    let editor = workspace_data.palette.input_editor.clone();
    let config = workspace_data.common.config;
    let focus = workspace_data.common.focus;
    let is_focused = move || focus.get() == Focus::Palette;

    let input = TextInputBuilder::new()
        .is_focused(is_focused)
        .build_editor(editor)
        .placeholder(move || workspace_data.palette.placeholder_text().to_owned())
        .style(|s| s.width_full());

    container(container(input).style(move |s| {
        let config = config.get();
        s.width_full()
            .height(25.0)
            .items_center()
            .border_bottom(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
    }))
    .style(|s| s.padding_bottom(5.0))
}

struct PaletteItems(im::Vector<PaletteItem>);

impl VirtualVector<(usize, PaletteItem)> for PaletteItems {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, PaletteItem)> {
        let start = range.start;
        Box::new(
            self.0
                .slice(range)
                .into_iter()
                .enumerate()
                .map(move |(i, item)| (i + start, item)),
        )
    }
}

fn palette_content(
    workspace_data: Rc<WorkspaceData>,
    layout_rect: ReadSignal<Rect>,
) -> impl View {
    let items = workspace_data.palette.filtered_items;
    let index = workspace_data.palette.index.read_only();
    let clicked_index = workspace_data.palette.clicked_index.write_only();
    let config = workspace_data.common.config;
    let run_id = workspace_data.palette.run_id;
    let input = workspace_data.palette.input.read_only();
    let palette_item_height = 25.0;
    stack((
        scroll({
            virtual_stack(
                move || PaletteItems(items.get()),
                move |(i, _item)| {
                    (run_id.get_untracked(), *i, input.get_untracked().input)
                },
                move |(i, item)| {
                    container(palette_item(
                        i,
                        item,
                        index,
                        palette_item_height,
                        config,
                    ))
                    .on_click_stop(move |_| {
                        clicked_index.set(Some(i));
                    })
                    .style(move |s| {
                        s.width_full().cursor(CursorStyle::Pointer).hover(|s| {
                            s.background(
                                config
                                    .get()
                                    .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                        })
                    })
                },
            )
            .item_size_fixed(move || palette_item_height)
            .style(|s| s.width_full().flex_col())
        })
        .ensure_visible(move || {
            Size::new(1.0, palette_item_height)
                .to_rect()
                .with_origin(Point::new(
                    0.0,
                    index.get() as f64 * palette_item_height,
                ))
        })
        .style(|s| {
            s.width_full()
                .min_height(0.0)
                .set(PropagatePointerWheel, false)
        }),
        text("No matching results").style(move |s| {
            s.display(if items.with(|items| items.is_empty()) {
                Display::Flex
            } else {
                Display::None
            })
            .padding_horiz(10.0)
            .align_items(Some(AlignItems::Center))
            .height(palette_item_height as f32)
        }),
    ))
    .style(move |s| {
        s.flex_col()
            .width_full()
            .min_height(0.0)
            .max_height((layout_rect.get().height() * 0.45 - 36.0).round() as f32)
            .padding_bottom(5.0)
            .padding_bottom(5.0)
    })
}

fn palette_preview(workspace_data: Rc<WorkspaceData>) -> impl View {
    let palette_data = workspace_data.palette.clone();
    let workspace = palette_data.workspace.clone();
    let preview_editor = palette_data.preview_editor;
    let has_preview = palette_data.has_preview;
    let config = palette_data.common.config;
    let preview_editor = create_rw_signal(preview_editor);
    container(
        container(editor_container_view(
            workspace_data,
            workspace,
            |_tracked: bool| true,
            preview_editor,
        ))
        .style(move |s| {
            let config = config.get();
            s.position(Position::Absolute)
                .border_top(1.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .size_full()
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
    )
    .style(move |s| {
        s.display(if has_preview.get() {
            Display::Flex
        } else {
            Display::None
        })
        .flex_grow(1.0)
    })
}

fn palette(workspace_data: Rc<WorkspaceData>) -> impl View {
    let layout_rect = workspace_data.layout_rect.read_only();
    let palette_data = workspace_data.palette.clone();
    let status = palette_data.status.read_only();
    let config = palette_data.common.config;
    let has_preview = palette_data.has_preview.read_only();
    container(
        stack((
            palette_input(workspace_data.clone()),
            palette_content(workspace_data.clone(), layout_rect),
            palette_preview(workspace_data.clone()),
        ))
        .on_event_stop(EventListener::PointerDown, move |_| {})
        .style(move |s| {
            let config = config.get();
            s.width(config.ui.palette_width() as f64)
                .max_width_full()
                .max_height(if has_preview.get() {
                    PxPctAuto::Auto
                } else {
                    PxPctAuto::Pct(100.0)
                })
                .height(if has_preview.get() {
                    PxPctAuto::Px(layout_rect.get().height() - 10.0)
                } else {
                    PxPctAuto::Auto
                })
                .margin_top(4.0)
                .border(1.0)
                .border_radius(6.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .flex_col()
                .background(config.color(LapceColor::PALETTE_BACKGROUND))
                .pointer_events_auto()
        }),
    )
    .style(move |s| {
        s.display(if status.get() == PaletteStatus::Inactive {
            Display::None
        } else {
            Display::Flex
        })
        .position(Position::Absolute)
        .size_full()
        .flex_col()
        .items_center()
        .pointer_events_none()
    })
    .debug_name("Pallete Layer")
}

fn window_message_view(
    messages: RwSignal<Vec<(String, ShowMessageParams)>>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let view_fn =
        move |(i, (title, message)): (usize, (String, ShowMessageParams))| {
            stack((
                svg(move || {
                    if let MessageType::ERROR = message.typ {
                        config.get().ui_svg(LapceIcons::ERROR)
                    } else {
                        config.get().ui_svg(LapceIcons::WARNING)
                    }
                })
                .style(move |s| {
                    let config = config.get();
                    let size = config.ui.icon_size() as f32;
                    let color = if let MessageType::ERROR = message.typ {
                        config.color(LapceColor::LAPCE_ERROR)
                    } else {
                        config.color(LapceColor::LAPCE_WARN)
                    };
                    s.min_width(size)
                        .size(size, size)
                        .margin_right(10.0)
                        .margin_top(4.0)
                        .color(color)
                }),
                stack((
                    text(title.clone()).style(|s| {
                        s.min_width(0.0).line_height(1.8).font_weight(Weight::BOLD)
                    }),
                    text(message.message.clone()).style(|s| {
                        s.min_width(0.0).line_height(1.8).margin_top(5.0)
                    }),
                ))
                .style(move |s| {
                    s.flex_col().min_width(0.0).flex_basis(0.0).flex_grow(1.0)
                }),
                clickable_icon(
                    || LapceIcons::CLOSE,
                    move || {
                        messages.update(|messages| {
                            messages.remove(i);
                        });
                    },
                    || false,
                    || false,
                    || "Close",
                    config,
                )
                .style(|s| s.margin_left(6.0)),
            ))
            .on_double_click_stop(move |_| {
                messages.update(|messages| {
                    messages.remove(i);
                });
            })
            .on_secondary_click_stop({
                let message = message.message.clone();
                move |_| {
                    let mut clipboard = SystemClipboard::new();
                    if !message.is_empty() {
                        clipboard.put_string(&message);
                    }
                }
            })
            .on_event_stop(EventListener::PointerDown, |_| {})
            .style(move |s| {
                let config = config.get();
                s.width_full()
                    .items_start()
                    .padding(10.0)
                    .border(1.0)
                    .border_radius(6.0)
                    .border_color(config.color(LapceColor::LAPCE_BORDER))
                    .background(config.color(LapceColor::PANEL_BACKGROUND))
                    .apply_if(i > 0, |s| s.margin_top(10.0))
            })
        };

    let id = AtomicU64::new(0);
    container(
        container(
            container(
                scroll(
                    dyn_stack(
                        move || messages.get().into_iter().enumerate(),
                        move |_| {
                            id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        },
                        view_fn,
                    )
                    .style(|s| s.flex_col().width_full()),
                )
                .style(|s| {
                    s.absolute()
                        .pointer_events_auto()
                        .width_full()
                        .min_height(0.0)
                        .max_height_full()
                        .set(PropagatePointerWheel, false)
                }),
            )
            .style(|s| s.size_full()),
        )
        .style(|s| {
            s.width(360.0)
                .max_width_pct(80.0)
                .padding(10.0)
                .height_full()
        }),
    )
    .style(|s| s.absolute().size_full().justify_end().pointer_events_none())
    .debug_name("Window Message View")
}

struct VectorItems<V>(im::Vector<V>);

impl<V: Clone + 'static> VirtualVector<(usize, V)> for VectorItems<V> {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(&mut self, range: Range<usize>) -> impl Iterator<Item = (usize, V)> {
        let start = range.start;
        self.0
            .slice(range)
            .into_iter()
            .enumerate()
            .map(move |(i, item)| (i + start, item))
    }
}

fn completion_kind_to_str(kind: CompletionItemKind) -> &'static str {
    match kind {
        CompletionItemKind::METHOD => "f",
        CompletionItemKind::FUNCTION => "f",
        CompletionItemKind::CLASS => "c",
        CompletionItemKind::STRUCT => "s",
        CompletionItemKind::VARIABLE => "v",
        CompletionItemKind::INTERFACE => "i",
        CompletionItemKind::ENUM => "e",
        CompletionItemKind::ENUM_MEMBER => "e",
        CompletionItemKind::FIELD => "v",
        CompletionItemKind::PROPERTY => "p",
        CompletionItemKind::CONSTANT => "d",
        CompletionItemKind::MODULE => "m",
        CompletionItemKind::KEYWORD => "k",
        CompletionItemKind::SNIPPET => "n",
        _ => "t",
    }
}

fn hover(workspace_data: Rc<WorkspaceData>) -> impl View {
    let hover_data = workspace_data.common.hover.clone();
    let config = workspace_data.common.config;
    let id = AtomicU64::new(0);
    let layout_rect = workspace_data.common.hover.layout_rect;

    scroll(
        dyn_stack(
            move || hover_data.content.get(),
            move |_| id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            move |content| match content {
                MarkdownContent::Text(text_layout) => container(
                    rich_text(move || text_layout.clone())
                        .style(|s| s.max_width(600.0)),
                )
                .style(|s| s.max_width_full()),
                MarkdownContent::Image { .. } => container(empty()),
                MarkdownContent::Separator => container(empty().style(move |s| {
                    s.width_full()
                        .margin_vert(5.0)
                        .height(1.0)
                        .background(config.get().color(LapceColor::LAPCE_BORDER))
                })),
            },
        )
        .style(|s| s.flex_col().padding_horiz(10.0).padding_vert(5.0)),
    )
    .on_resize(move |rect| {
        layout_rect.set(rect);
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .on_event_stop(EventListener::PointerDown, |_| {})
    .style(move |s| {
        let active = workspace_data.common.hover.active.get();
        if !active {
            s.hide()
        } else {
            let config = config.get();
            if let Some(origin) = workspace_data.hover_origin() {
                s.absolute()
                    .margin_left(origin.x as f32)
                    .margin_top(origin.y as f32)
                    .max_height(300.0)
                    .border(1.0)
                    .border_radius(6.0)
                    .border_color(config.color(LapceColor::LAPCE_BORDER))
                    .background(config.color(LapceColor::PANEL_BACKGROUND))
                    .set(PropagatePointerWheel, false)
            } else {
                s.hide()
            }
        }
    })
    .debug_name("Hover Layer")
}

fn completion(workspace_data: Rc<WorkspaceData>) -> impl View {
    let completion_data = workspace_data.common.completion;
    let active_editor = workspace_data.main_split.active_editor;
    let config = workspace_data.common.config;
    let active = completion_data.with_untracked(|c| c.active);
    let request_id =
        move || completion_data.with_untracked(|c| (c.request_id, c.input_id));
    scroll(
        virtual_stack(
            move || completion_data.with(|c| VectorItems(c.filtered_items.clone())),
            move |(i, _item)| (request_id(), *i),
            move |(i, item)| {
                stack((
                    container(
                        text(
                            item.item.kind.map(completion_kind_to_str).unwrap_or(""),
                        )
                        .style(move |s| {
                            s.width_full()
                                .justify_content(Some(JustifyContent::Center))
                        }),
                    )
                    .style(move |s| {
                        let config = config.get();
                        let width = config.editor.line_height() as f32;
                        s.width(width)
                            .min_width(width)
                            .height_full()
                            .align_items(Some(AlignItems::Center))
                            .font_weight(Weight::BOLD)
                            .apply_opt(
                                config.completion_color(item.item.kind),
                                |s, c| s.color(c).background(c.multiply_alpha(0.3)),
                            )
                    }),
                    focus_text(
                        move || {
                            if config.get().editor.completion_item_show_detail {
                                item.item
                                    .detail
                                    .clone()
                                    .unwrap_or(item.item.label.clone())
                            } else {
                                item.item.label.clone()
                            }
                        },
                        move || item.indices.clone(),
                        move || config.get().color(LapceColor::EDITOR_FOCUS),
                    )
                    .on_click_stop(move |_| {
                        active.set(i);
                        if let Some(editor) = active_editor.get_untracked() {
                            editor.select_completion();
                        }
                    })
                    .on_event_stop(EventListener::PointerDown, |_| {})
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(5.0)
                            .min_width(0.0)
                            .align_items(Some(AlignItems::Center))
                            .size_full()
                            .cursor(CursorStyle::Pointer)
                            .apply_if(active.get() == i, |s| {
                                s.background(
                                    config.color(LapceColor::COMPLETION_CURRENT),
                                )
                            })
                            .hover(move |s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    }),
                ))
                .style(move |s| {
                    s.align_items(Some(AlignItems::Center))
                        .width_full()
                        .height(config.get().editor.line_height() as f32)
                })
            },
        )
        .item_size_fixed(move || config.get().editor.line_height() as f64)
        .style(|s| {
            s.align_items(Some(AlignItems::Center))
                .width_full()
                .flex_col()
        }),
    )
    .ensure_visible(move || {
        let config = config.get();
        let active = active.get();
        Size::new(1.0, config.editor.line_height() as f64)
            .to_rect()
            .with_origin(Point::new(
                0.0,
                active as f64 * config.editor.line_height() as f64,
            ))
    })
    .on_resize(move |rect| {
        completion_data.update(|c| {
            c.layout_rect = rect;
        });
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .style(move |s| {
        let config = config.get();
        let origin = workspace_data.completion_origin();
        s.position(Position::Absolute)
            .width(config.editor.completion_width as i32)
            .max_height(400.0)
            .margin_left(origin.x as f32)
            .margin_top(origin.y as f32)
            .background(config.color(LapceColor::COMPLETION_BACKGROUND))
            .font_family(config.editor.font_family.clone())
            .font_size(config.editor.font_size() as f32)
            .border_radius(6.0)
    })
    .debug_name("Completion Layer")
}

fn code_action(workspace_data: Rc<WorkspaceData>) -> impl View {
    let config = workspace_data.common.config;
    let code_action = workspace_data.code_action;
    let (status, active) = code_action
        .with_untracked(|code_action| (code_action.status, code_action.active));
    let request_id =
        move || code_action.with_untracked(|code_action| code_action.request_id);
    scroll(
        container(
            dyn_stack(
                move || {
                    code_action.with(|code_action| {
                        code_action.filtered_items.clone().into_iter().enumerate()
                    })
                },
                move |(i, _item)| (request_id(), *i),
                move |(i, item)| {
                    container(
                        text(item.title().replace('\n', " "))
                            .style(|s| s.text_ellipsis().min_width(0.0)),
                    )
                    .on_click_stop(move |_| {
                        let code_action = code_action.get_untracked();
                        code_action.active.set(i);
                        code_action.select();
                    })
                    .on_event_stop(EventListener::PointerDown, |_| {})
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(10.0)
                            .align_items(Some(AlignItems::Center))
                            .min_width(0.0)
                            .width_full()
                            .line_height(1.8)
                            .border_radius(6.0)
                            .cursor(CursorStyle::Pointer)
                            .apply_if(active.get() == i, |s| {
                                s.background(
                                    config.color(LapceColor::COMPLETION_CURRENT),
                                )
                            })
                            .hover(move |s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    })
                },
            )
            .style(|s| s.width_full().flex_col()),
        )
        .style(|s| s.width_full().padding_vert(4.0)),
    )
    .ensure_visible(move || {
        let config = config.get();
        let active = active.get();
        Size::new(1.0, config.editor.line_height() as f64)
            .to_rect()
            .with_origin(Point::new(
                0.0,
                active as f64 * config.editor.line_height() as f64,
            ))
    })
    .on_resize(move |rect| {
        code_action.update(|c| {
            c.layout_rect = rect;
        });
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .style(move |s| {
        let origin = workspace_data.code_action_origin();
        s.display(match status.get() {
            CodeActionStatus::Inactive => Display::None,
            CodeActionStatus::Active => Display::Flex,
        })
        .position(Position::Absolute)
        .width(400.0)
        .max_height(400.0)
        .margin_left(origin.x as f32)
        .margin_top(origin.y as f32)
        .background(config.get().color(LapceColor::COMPLETION_BACKGROUND))
        .border_radius(6.0)
    })
    .debug_name("Code Action Layer")
}

fn rename(workspace_data: Rc<WorkspaceData>) -> impl View {
    let editor = workspace_data.rename.editor.clone();
    let active = workspace_data.rename.active;
    let layout_rect = workspace_data.rename.layout_rect;
    let config = workspace_data.common.config;

    container(
        container(
            TextInputBuilder::new()
                .is_focused(move || active.get())
                .build_editor(editor)
                .style(|s| s.width(150.0)),
        )
        .style(move |s| {
            let config = config.get();
            s.font_family(config.editor.font_family.clone())
                .font_size(config.editor.font_size() as f32)
                .border(1.0)
                .border_radius(6.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
    )
    .on_resize(move |rect| {
        layout_rect.set(rect);
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .on_event_stop(EventListener::PointerDown, |_| {})
    .style(move |s| {
        let origin = workspace_data.rename_origin();
        s.position(Position::Absolute)
            .apply_if(!active.get(), |s| s.hide())
            .margin_left(origin.x as f32)
            .margin_top(origin.y as f32)
            .background(config.get().color(LapceColor::PANEL_BACKGROUND))
            .border_radius(6.0)
            .padding(6.0)
    })
    .debug_name("Rename Layer")
}

fn workspace_view(workspace_data: Rc<WorkspaceData>) -> impl View {
    let window_origin = workspace_data.common.window_origin;
    let layout_rect = workspace_data.layout_rect;
    let config = workspace_data.common.config;
    let workspace_scope = workspace_data.scope;
    let hover_active = workspace_data.common.hover.active;

    let view = if workspace_data.workspace.path.is_none() {
        empty_workspace_view(workspace_data.clone()).into_any()
    } else {
        let status_height = workspace_data.status_height;
        stack((
            stack((
                title(workspace_data.clone()),
                workbench(workspace_data.clone()),
                status(workspace_data.clone(), status_height, config),
            ))
            .on_resize(move |rect| {
                layout_rect.set(rect);
            })
            .on_move(move |point| {
                window_origin.set(point);
            })
            .style(|s| s.size_full().flex_col())
            .debug_name("Base Layer"),
            completion(workspace_data.clone()),
            hover(workspace_data.clone()),
            code_action(workspace_data.clone()),
            rename(workspace_data.clone()),
            palette(workspace_data.clone()),
            crate::search_modal::search_modal_popup(workspace_data.clone()),
            recent_files::recent_files_popup(workspace_data.clone()),
            about::about_popup(workspace_data.clone()),
            crate::panel::plugin_view::plugin_popup(workspace_data.clone()),
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
            s.size_full()
                .color(config.color(LapceColor::EDITOR_FOREGROUND))
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
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
    let workspaces = window_data.workspaces.read_only();
    let active = window_data.active.read_only();
    let items = move || workspaces.get();
    let key = |(_, workspace): &(RwSignal<usize>, Rc<WorkspaceData>)| {
        workspace.workspace_id
    };
    let active = move || active.get();
    let window_focus = create_rw_signal(false);
    let ime_enabled = window_data.ime_enabled;
    let window_maximized = window_data.common.window_maximized;

    tab(active, items, key, |(_, workspace_data)| {
        workspace_view(workspace_data)
    })
    .window_title(move || {
        let active = active();
        let workspaces = workspaces.get();
        let workspace = workspaces
            .get(active)
            .or_else(|| workspaces.last())
            .and_then(|(_, workspace)| workspace.workspace.display());
        match workspace {
            Some(workspace) => format!("{workspace} - Lapce"),
            None => "Lapce".to_string(),
        }
    })
    .on_event_stop(EventListener::ImeEnabled, move |_| {
        ime_enabled.set(true);
    })
    .on_event_stop(EventListener::ImeDisabled, move |_| {
        ime_enabled.set(false);
    })
    .on_event_cont(EventListener::WindowGotFocus, move |_| {
        window_focus.set(true);
    })
    .on_event_cont(EventListener::WindowMaximizeChanged, move |event| {
        if let Event::WindowMaximizeChanged(maximized) = event {
            window_maximized.set(*maximized);
        }
    })
    .window_menu(move || {
        window_focus.track();
        let active = active();
        let workspaces = workspaces.get();
        let workspace = workspaces.get(active).or_else(|| workspaces.last());
        if let Some((_, workspace)) = workspace {
            workspace.common.keypress.track();
            let workbench_command = workspace.common.workbench_command;
            let lapce_command = workspace.common.lapce_command;
            window_menu(lapce_command, workbench_command)
        } else {
            Menu::new("Lapce")
        }
    })
    .style(|s| s.size_full())
    .debug_name("Window")
}

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

        FONT_SYSTEM
            .lock()
            .db_mut()
            .load_font_source(Source::Binary(Arc::new(FONT_DEJAVU_SANS_REGULAR)));
        FONT_SYSTEM
            .lock()
            .db_mut()
            .load_font_source(Source::Binary(Arc::new(
                FONT_DEJAVU_SANS_MONO_REGULAR,
            )));
    }

    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        trace!(TraceLevel::INFO, "Loading custom environment from shell");
        load_shell_env();
    }

    // small hack to unblock terminal if launched from it
    // launch it as a separate process that waits
    if !cli.wait {
        let mut args = std::env::args().collect::<Vec<_>>();
        args.push("--wait".to_string());
        let mut cmd = std::process::Command::new(&args[0]);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW);

        let stderr_file_path =
            Directory::logs_directory().unwrap().join("stderr.log");
        let stderr_file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .read(true)
            .open(stderr_file_path)
            .unwrap();
        let stderr = Stdio::from(stderr_file);

        let stdout_file_path =
            Directory::logs_directory().unwrap().join("stdout.log");
        let stdout_file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .read(true)
            .open(stdout_file_path)
            .unwrap();
        let stdout = Stdio::from(stdout_file);

        if let Err(why) = cmd
            .args(&args[1..])
            .stderr(stderr)
            .stdout(stdout)
            .env("LAPCE_LOG", "lapce_app::app=error,off")
            .spawn()
        {
            eprintln!("Failed to launch lapce: {why}");
            std::process::exit(1);
        };
        return;
    }

    // If the cli is not requesting a new window, and we're not developing a plugin, we try to open
    // in the existing Lapce process
    if !cli.new {
        match get_socket() {
            Ok(socket) => {
                if let Err(e) = try_open_in_existing_process(socket, &cli.paths) {
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

    let plugin_paths = Arc::new(cli.plugin_path);

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(ConfigWatcher::new(tx)).unwrap();
    if let Some(path) = LapceConfig::settings_file() {
        if let Err(err) = watcher.watch(&path, notify::RecursiveMode::Recursive) {
            tracing::error!("{:?}", err);
        }
    }
    if let Some(path) = Directory::themes_directory() {
        if let Err(err) = watcher.watch(&path, notify::RecursiveMode::Recursive) {
            tracing::error!("{:?}", err);
        }
    }
    if let Some(path) = LapceConfig::keymaps_file() {
        if let Err(err) = watcher.watch(&path, notify::RecursiveMode::Recursive) {
            tracing::error!("{:?}", err);
        }
    }
    if let Some(path) = Directory::plugins_directory() {
        if let Err(err) = watcher.watch(&path, notify::RecursiveMode::Recursive) {
            tracing::error!("{:?}", err);
        }
    }

    install_bundled_plugins();

    let windows = scope.create_rw_signal(im::HashMap::new());
    let config = LapceConfig::load(&LapceWorkspace::default(), &[], &plugin_paths);

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
        plugin_paths,
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
                    for (_, tab) in window.workspaces.get_untracked() {
                        for (_, doc) in tab.main_split.docs.get_untracked() {
                            doc.syntax.update(|syntaxt| {
                                *syntaxt = Syntax::from_language(syntaxt.language);
                            });
                            doc.trigger_syntax_change(None);
                        }
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
                if let Err(err) = listen_local_socket(tx) {
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

/// Uses a login shell to load the correct shell environment for the current user.
pub fn load_shell_env() {
    use std::process::Command;

    use tracing::warn;

    #[cfg(not(windows))]
    let shell = match std::env::var("SHELL") {
        Ok(s) => s,
        Err(error) => {
            // Shell variable is not set, so we can't determine the correct shell executable.
            trace!(
                TraceLevel::ERROR,
                "Failed to obtain shell environment: {error}"
            );
            return;
        }
    };

    #[cfg(windows)]
    let shell = "powershell";

    let mut command = Command::new(shell);

    #[cfg(not(windows))]
    command.args(["--login", "-c", "printenv"]);

    #[cfg(windows)]
    command.args([
        "-Command",
        "Get-ChildItem env: | ForEach-Object { \"{0}={1}\" -f $_.Name, $_.Value }",
    ]);

    #[cfg(windows)]
    command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW);

    let env = match command.output() {
        Ok(output) => String::from_utf8(output.stdout).unwrap_or_default(),

        Err(error) => {
            trace!(
                TraceLevel::ERROR,
                "Failed to obtain shell environment: {error}"
            );
            return;
        }
    };

    env.split('\n')
        .filter_map(|line| line.split_once('='))
        .for_each(|(key, value)| unsafe {
            let value = value.trim_matches('\r');
            if let Ok(v) = std::env::var(key) {
                if v != value {
                    warn!("Overwriting '{key}', previous value: '{v}', new value '{value}'");
                }
            };
            std::env::set_var(key, value);
        })
}

pub fn get_socket() -> Result<interprocess::local_socket::LocalSocketStream> {
    let local_socket = Directory::local_socket()
        .ok_or_else(|| anyhow!("can't get local socket folder"))?;
    let socket =
        interprocess::local_socket::LocalSocketStream::connect(local_socket)?;
    Ok(socket)
}

pub fn try_open_in_existing_process(
    mut socket: interprocess::local_socket::LocalSocketStream,
    paths: &[PathObject],
) -> Result<()> {
    let msg: CoreMessage = RpcMessage::Notification(CoreNotification::OpenPaths {
        paths: paths.to_vec(),
    });
    lapce_rpc::stdio::write_msg(&mut socket, msg)?;

    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let mut buf = [0; 100];
        let received = if let Ok(n) = socket.read(&mut buf) {
            &buf[..n] == b"received"
        } else {
            false
        };
        tx.send(received)
    });

    let received = rx.recv_timeout(std::time::Duration::from_millis(500))?;
    if !received {
        return Err(anyhow!("didn't receive response"));
    }

    Ok(())
}

fn listen_local_socket(tx: SyncSender<CoreNotification>) -> Result<()> {
    let local_socket = Directory::local_socket()
        .ok_or_else(|| anyhow!("can't get local socket folder"))?;
    if local_socket.exists() {
        if let Err(err) = std::fs::remove_file(&local_socket) {
            tracing::error!("{:?}", err);
        }
    }
    let socket =
        interprocess::local_socket::LocalSocketListener::bind(local_socket)?;

    for stream in socket.incoming().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || -> Result<()> {
            let mut reader = BufReader::new(stream);
            loop {
                let msg: Option<CoreMessage> =
                    lapce_rpc::stdio::read_msg(&mut reader)?;

                if let Some(RpcMessage::Notification(msg)) = msg {
                    tx.send(msg)?;
                } else {
                    trace!(TraceLevel::ERROR, "Unhandled message: {msg:?}");
                }

                let stream_ref = reader.get_mut();
                if let Err(err) = stream_ref.write_all(b"received") {
                    tracing::error!("{:?}", err);
                }
                if let Err(err) = stream_ref.flush() {
                    tracing::error!("{:?}", err);
                }
            }
        });
    }
    Ok(())
}

pub fn window_menu(
    lapce_command: Listener<LapceCommand>,
    workbench_command: Listener<LapceWorkbenchCommand>,
) -> Menu {
    let file_menu = Menu::new("File")
        .entry(MenuItem::new("Open Folder").action(move || {
            workbench_command.send(LapceWorkbenchCommand::OpenFolder);
        }))
        .entry(MenuItem::new("Open Recent Workspace").action(move || {
            workbench_command.send(LapceWorkbenchCommand::PaletteWorkspace);
        }));

    let view_menu = Menu::new("View")
        .entry(MenuItem::new("Toggle Left Panel").action(move || {
            workbench_command.send(LapceWorkbenchCommand::TogglePanelLeftVisual);
        }))
        .entry(MenuItem::new("Toggle Bottom Panel").action(move || {
            workbench_command.send(LapceWorkbenchCommand::TogglePanelBottomVisual);
        }))
        .entry(MenuItem::new("Toggle Right Panel").action(move || {
            workbench_command.send(LapceWorkbenchCommand::TogglePanelRightVisual);
        }))
        .separator()
        .entry(MenuItem::new("Toggle Inlay Hints").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ToggleInlayHints);
        }))
        .entry(MenuItem::new("Reset Zoom").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ZoomReset);
        }))
        .separator()
        .entry(MenuItem::new("Reveal Active File in File Explorer").action(
            move || {
                workbench_command
                    .send(LapceWorkbenchCommand::RevealActiveFileInFileExplorer);
            },
        ));

    let code_menu = Menu::new("Code")
        .entry(MenuItem::new("Go to Definition").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::GotoDefinition),
                data: None,
            });
        }))
        .entry(MenuItem::new("Go to Type Definition").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::GotoTypeDefinition),
                data: None,
            });
        }))
        .entry(MenuItem::new("Go to Implementation").action(move || {
            workbench_command.send(LapceWorkbenchCommand::GoToImplementation);
        }))
        .entry(MenuItem::new("Find References").action(move || {
            workbench_command.send(LapceWorkbenchCommand::FindReferences);
        }))
        .separator()
        .entry(MenuItem::new("Show Hover").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::ShowHover),
                data: None,
            });
        }))
        .entry(MenuItem::new("Show Code Actions").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::ShowCodeActions),
                data: None,
            });
        }))
        .entry(MenuItem::new("Show Call Hierarchy").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ShowCallHierarchy);
        }))
        .separator()
        .entry(MenuItem::new("Rename Symbol").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::Rename),
                data: None,
            });
        }))
        .entry(MenuItem::new("Format Document").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::FormatDocument),
                data: None,
            });
        }));

    let window_menu = Menu::new("Window")
        .entry(MenuItem::new("New Window").action(move || {
            workbench_command.send(LapceWorkbenchCommand::NewWindow);
        }))
        .separator()
        .entry(MenuItem::new("Reload Window").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ReloadWindow);
        }));

    let settings_menu = Menu::new("Settings")
        .entry(MenuItem::new("Open Settings").action(move || {
            workbench_command.send(LapceWorkbenchCommand::OpenSettings);
        }))
        .entry(MenuItem::new("Open Keyboard Shortcuts").action(move || {
            workbench_command.send(LapceWorkbenchCommand::OpenKeyboardShortcuts);
        }))
        .separator()
        .entry(MenuItem::new("Change Color Theme").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ChangeColorTheme);
        }))
        .entry(MenuItem::new("Change Icon Theme").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ChangeIconTheme);
        }))
        .entry(MenuItem::new("Open Theme Color Settings").action(move || {
            workbench_command.send(LapceWorkbenchCommand::OpenThemeColorSettings);
        }))
        .separator()
        .entry(MenuItem::new("Export Theme Settings").action(move || {
            workbench_command
                .send(LapceWorkbenchCommand::ExportCurrentThemeSettings);
        }))
        .entry(MenuItem::new("Install Theme").action(move || {
            workbench_command.send(LapceWorkbenchCommand::InstallTheme);
        }))
        .separator()
        .entry(MenuItem::new("Plugins").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ShowPlugins);
        }));

    let help_menu = {
        let mut menu = Menu::new("Help")
            .entry(MenuItem::new("Open Log File").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenLogFile);
            }))
            .entry(MenuItem::new("Open Logs Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenLogsDirectory);
            }))
            .separator()
            .entry(MenuItem::new("Open Settings Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenSettingsDirectory);
            }))
            .entry(MenuItem::new("Open Settings File").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenSettingsFile);
            }))
            .entry(
                MenuItem::new("Open Keyboard Shortcuts File").action(move || {
                    workbench_command
                        .send(LapceWorkbenchCommand::OpenKeyboardShortcutsFile);
                }),
            )
            .separator()
            .entry(MenuItem::new("Open Plugins Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenPluginsDirectory);
            }))
            .entry(MenuItem::new("Open Themes Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenThemesDirectory);
            }))
            .entry(MenuItem::new("Open Grammars Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenGrammarsDirectory);
            }))
            .entry(MenuItem::new("Open Queries Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenQueriesDirectory);
            }))
            .entry(MenuItem::new("Open Proxy Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenProxyDirectory);
            }));
        #[cfg(target_os = "macos")]
        {
            menu = menu
                .separator()
                .entry(MenuItem::new("Install to PATH").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::InstallToPATH);
                }))
                .entry(MenuItem::new("Uninstall from PATH").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::UninstallFromPATH);
                }));
        }
        menu.separator()
            .entry(MenuItem::new("Show Environment").action(move || {
                workbench_command.send(LapceWorkbenchCommand::ShowEnvironment);
            }))
            .entry(MenuItem::new("Open UI Inspector").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenUIInspector);
            }))
    };

    Menu::new("Lapce")
        .entry({
            let mut menu = Menu::new("Lapce")
                .entry(MenuItem::new("About Lapce").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::ShowAbout)
                }))
                .separator()
                .entry(MenuItem::new("Quit Lapce").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::Quit);
                }));
            if cfg!(target_os = "macos") {
                menu = menu
                    .separator()
                    .entry(MenuItem::new("Hide Lapce"))
                    .entry(MenuItem::new("Hide Others"))
                    .entry(MenuItem::new("Show All"))
            }
            menu
        })
        .separator()
        .entry(file_menu)
        .entry(view_menu)
        .entry(code_menu)
        .entry(window_menu)
        .entry(settings_menu)
        .entry(help_menu)
}
fn tab_secondary_click(
    internal_command: Listener<InternalCommand>,
    editor_tab_id: EditorTabId,
    child: EditorTabChild,
) {
    let mut menu = Menu::new("");
    let child_other = child.clone();
    let child_right = child.clone();
    let child_left = child.clone();
    menu = menu
        .entry(MenuItem::new("Close").action(move || {
            internal_command.send(InternalCommand::EditorTabChildClose {
                editor_tab_id,
                child: child.clone(),
            });
        }))
        .entry(MenuItem::new("Close Other Tabs").action(move || {
            internal_command.send(InternalCommand::EditorTabCloseByKind {
                editor_tab_id,
                child: child_other.clone(),
                kind: TabCloseKind::CloseOther,
            });
        }))
        .entry(MenuItem::new("Close All Tabs").action(move || {
            internal_command.send(InternalCommand::EditorTabClose { editor_tab_id });
        }))
        .entry(MenuItem::new("Close Tabs to the Right").action(move || {
            internal_command.send(InternalCommand::EditorTabCloseByKind {
                editor_tab_id,
                child: child_right.clone(),
                kind: TabCloseKind::CloseToRight,
            });
        }))
        .entry(MenuItem::new("Close Tabs to the Left").action(move || {
            internal_command.send(InternalCommand::EditorTabCloseByKind {
                editor_tab_id,
                child: child_left.clone(),
                kind: TabCloseKind::CloseToLeft,
            });
        }));
    show_context_menu(menu, None);
}
