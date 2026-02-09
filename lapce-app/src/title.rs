use std::{rc::Rc, sync::Arc};

use floem::{
    View,
    event::EventListener,
    menu::{Menu, MenuItem},
    reactive::{
        Memo, ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith, create_memo,
    },
    style::{AlignItems, JustifyContent},
    views::{Decorators, container, drag_window_area, empty, label, stack, svg},
};
use lapce_core::meta;

use crate::{
    app::{clickable_icon, not_clickable_icon, window_menu},
    command::{LapceCommand, LapceWorkbenchCommand, WindowCommand},
    config::{LapceConfig, color::LapceColor, icon::LapceIcons},
    listener::Listener,
    update::ReleaseInfo,
    window_tab::WindowTabData,
    workspace::LapceWorkspace,
};

fn left(
    workspace: Arc<LapceWorkspace>,
    lapce_command: Listener<LapceCommand>,
    workbench_command: Listener<LapceWorkbenchCommand>,
    config: ReadSignal<Arc<LapceConfig>>,
    num_window_tabs: Memo<usize>,
) -> impl View {
    let is_macos = cfg!(target_os = "macos");
    let has_folder = workspace.path.is_some();
    stack((
        empty().style(move |s| {
            let should_hide = if is_macos {
                num_window_tabs.get() > 1
            } else {
                true
            };
            s.width(75.0).apply_if(should_hide, |s| s.hide())
        }),
        container(svg(move || config.get().ui_svg(LapceIcons::LOGO)).style(
            move |s| {
                let config = config.get();
                s.size(16.0, 16.0)
                    .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
            },
        ))
        .style(move |s| s.margin_horiz(10.0).apply_if(is_macos, |s| s.hide())),
        not_clickable_icon(
            || LapceIcons::MENU,
            || false,
            || false,
            || "Menu",
            config,
        )
        .popout_menu(move || {
            window_menu(lapce_command, workbench_command, has_folder)
        })
        .style(move |s| {
            s.margin_left(4.0)
                .margin_right(6.0)
                .apply_if(is_macos, |s| s.hide())
        }),
        drag_window_area(empty())
            .style(|s| s.height_pct(100.0).flex_basis(0.0).flex_grow(1.0)),
    ))
    .style(move |s| {
        s.height_pct(100.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .items_center()
    })
    .debug_name("Left Side of Top Bar")
}

fn middle(
    workspace: Arc<LapceWorkspace>,
    workbench_command: Listener<LapceWorkbenchCommand>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let local_workspace = workspace.clone();
    let open_folder = move || {
        not_clickable_icon(
            || LapceIcons::PALETTE_MENU,
            || false,
            || false,
            || "Open Folder / Recent Workspace",
            config,
        )
        .popout_menu(move || {
            Menu::new("")
                .entry(MenuItem::new("Open Folder").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::OpenFolder);
                }))
                .entry(MenuItem::new("Open Recent Workspace").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::PaletteWorkspace);
                }))
        })
    };

    stack((
        stack((drag_window_area(empty())
            .style(|s| s.height_pct(100.0).flex_basis(0.0).flex_grow(1.0)),))
        .style(|s| {
            s.flex_basis(0)
                .flex_grow(1.0)
                .justify_content(Some(JustifyContent::FlexEnd))
        }),
        container(
            stack((
                svg(move || config.get().ui_svg(LapceIcons::SEARCH)).style(
                    move |s| {
                        let config = config.get();
                        let icon_size = config.ui.icon_size() as f32;
                        s.size(icon_size, icon_size)
                            .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    },
                ),
                label(move || {
                    if let Some(s) = local_workspace.display() {
                        s
                    } else {
                        "Open Folder".to_string()
                    }
                })
                .style(|s| s.padding_left(10).padding_right(5).selectable(false)),
                open_folder(),
            ))
            .style(|s| s.align_items(Some(AlignItems::Center))),
        )
        .on_event_stop(EventListener::PointerDown, |_| {})
        .on_click_stop(move |_| {
            if workspace.clone().path.is_some() {
                workbench_command.send(LapceWorkbenchCommand::PaletteHelpAndFile);
            } else {
                workbench_command.send(LapceWorkbenchCommand::PaletteWorkspace);
            }
        })
        .style(move |s| {
            let config = config.get();
            s.flex_basis(0)
                .flex_grow(10.0)
                .min_width(200.0)
                .max_width(500.0)
                .height(26.0)
                .justify_content(Some(JustifyContent::Center))
                .align_items(Some(AlignItems::Center))
                .border(1.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .border_radius(6.0)
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
        drag_window_area(empty())
            .style(|s| s.height_pct(100.0).flex_basis(0.0).flex_grow(1.0)),
    ))
    .style(|s| {
        s.flex_basis(0)
            .flex_grow(2.0)
            .align_items(Some(AlignItems::Center))
            .justify_content(Some(JustifyContent::Center))
    })
    .debug_name("Middle of Top Bar")
}

fn right(
    window_command: Listener<WindowCommand>,
    workbench_command: Listener<LapceWorkbenchCommand>,
    latest_release: ReadSignal<Arc<Option<ReleaseInfo>>>,
    update_in_progress: RwSignal<bool>,
    num_window_tabs: Memo<usize>,
    window_maximized: RwSignal<bool>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let latest_version = create_memo(move |_| {
        let latest_release = latest_release.get();
        let latest_version =
            latest_release.as_ref().as_ref().map(|r| r.version.clone());
        if latest_version.is_some()
            && latest_version.as_deref() != Some(meta::VERSION)
        {
            latest_version
        } else {
            None
        }
    });

    let has_update = move || latest_version.with(|v| v.is_some());

    stack((
        drag_window_area(empty())
            .style(|s| s.height_pct(100.0).flex_basis(0.0).flex_grow(1.0)),
        stack((
            not_clickable_icon(
                || LapceIcons::SETTINGS,
                || false,
                || false,
                || "Settings",
                config,
            )
            .popout_menu(move || {
                Menu::new("")
                    .entry(MenuItem::new("Command Palette").action(move || {
                        workbench_command.send(LapceWorkbenchCommand::PaletteCommand)
                    }))
                    .separator()
                    .entry(MenuItem::new("Open Settings").action(move || {
                        workbench_command.send(LapceWorkbenchCommand::OpenSettings)
                    }))
                    .entry(MenuItem::new("Open Keyboard Shortcuts").action(
                        move || {
                            workbench_command
                                .send(LapceWorkbenchCommand::OpenKeyboardShortcuts)
                        },
                    ))
                    .entry(MenuItem::new("Open Theme Color Settings").action(
                        move || {
                            workbench_command
                                .send(LapceWorkbenchCommand::OpenThemeColorSettings)
                        },
                    ))
                    .separator()
                    .entry(if let Some(v) = latest_version.get_untracked() {
                        if update_in_progress.get_untracked() {
                            MenuItem::new(format!("Update in progress ({v})"))
                                .enabled(false)
                        } else {
                            MenuItem::new(format!("Restart to update ({v})")).action(
                                move || {
                                    workbench_command
                                        .send(LapceWorkbenchCommand::RestartToUpdate)
                                },
                            )
                        }
                    } else {
                        MenuItem::new("No update available").enabled(false)
                    })
                    .separator()
                    .entry(MenuItem::new("About Lapce").action(move || {
                        workbench_command.send(LapceWorkbenchCommand::ShowAbout)
                    }))
            }),
            container(label(|| "1".to_string()).style(move |s| {
                let config = config.get();
                s.font_size(10.0)
                    .color(config.color(LapceColor::EDITOR_BACKGROUND))
                    .border_radius(100.0)
                    .margin_left(5.0)
                    .margin_top(10.0)
                    .background(config.color(LapceColor::EDITOR_CARET))
            }))
            .style(move |s| {
                let has_update = has_update();
                s.absolute()
                    .size_pct(100.0, 100.0)
                    .justify_end()
                    .items_end()
                    .pointer_events_none()
                    .apply_if(!has_update, |s| s.hide())
            }),
        ))
        .style(move |s| s.margin_horiz(6.0)),
        window_controls_view(
            window_command,
            true,
            num_window_tabs,
            window_maximized,
            config,
        ),
    ))
    .style(|s| {
        s.flex_basis(0)
            .flex_grow(1.0)
            .justify_content(Some(JustifyContent::FlexEnd))
    })
    .debug_name("Right of top bar")
}

pub fn title(window_tab_data: Rc<WindowTabData>) -> impl View {
    let workspace = window_tab_data.workspace.clone();
    let lapce_command = window_tab_data.common.lapce_command;
    let workbench_command = window_tab_data.common.workbench_command;
    let window_command = window_tab_data.common.window_common.window_command;
    let latest_release = window_tab_data.common.window_common.latest_release;
    let num_window_tabs = window_tab_data.common.window_common.num_window_tabs;
    let window_maximized = window_tab_data.common.window_common.window_maximized;
    let title_height = window_tab_data.title_height;
    let update_in_progress = window_tab_data.update_in_progress;
    let config = window_tab_data.common.config;
    stack((
        left(
            workspace.clone(),
            lapce_command,
            workbench_command,
            config,
            num_window_tabs,
        ),
        middle(workspace, workbench_command, config),
        right(
            window_command,
            workbench_command,
            latest_release,
            update_in_progress,
            num_window_tabs,
            window_maximized,
            config,
        ),
    ))
    .on_resize(move |rect| {
        let height = rect.height();
        if height != title_height.get_untracked() {
            title_height.set(height);
        }
    })
    .style(move |s| {
        let config = config.get();
        s.width_pct(100.0)
            .height(37.0)
            .items_center()
            .background(config.color(LapceColor::PANEL_BACKGROUND))
            .border_bottom(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
    })
    .debug_name("Title / Top Bar")
}

pub fn window_controls_view(
    window_command: Listener<WindowCommand>,
    is_title: bool,
    num_window_tabs: Memo<usize>,
    window_maximized: RwSignal<bool>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    stack((
        clickable_icon(
            || LapceIcons::WINDOW_MINIMIZE,
            || {
                floem::action::minimize_window();
            },
            || false,
            || false,
            || "Minimize",
            config,
        )
        .style(|s| s.margin_right(16.0).margin_left(10.0)),
        clickable_icon(
            move || {
                if window_maximized.get() {
                    LapceIcons::WINDOW_RESTORE
                } else {
                    LapceIcons::WINDOW_MAXIMIZE
                }
            },
            move || {
                floem::action::set_window_maximized(
                    !window_maximized.get_untracked(),
                );
            },
            || false,
            || false,
            || "Maximize",
            config,
        )
        .style(|s| s.margin_right(16.0)),
        clickable_icon(
            || LapceIcons::WINDOW_CLOSE,
            move || {
                window_command.send(WindowCommand::CloseWindow);
            },
            || false,
            || false,
            || "Close Window",
            config,
        )
        .style(|s| s.margin_right(6.0)),
    ))
    .style(move |s| {
        s.apply_if(
            cfg!(target_os = "macos")
                || !config.get_untracked().core.custom_titlebar
                || (is_title && num_window_tabs.get() > 1),
            |s| s.hide(),
        )
    })
}
