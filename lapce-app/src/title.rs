use std::{rc::Rc, sync::Arc};

use floem::{
    View,
    menu::{Menu, MenuItem},
    reactive::{
        ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith, create_memo,
    },
    style::JustifyContent,
    views::{Decorators, container, drag_window_area, empty, label, stack, svg},
};
use lapce_core::meta;

use lapce_rpc::core::GitRepoState;

use crate::{
    app::{clickable_icon, not_clickable_icon, window_menu},
    command::{LapceCommand, LapceWorkbenchCommand, WindowCommand},
    config::{LapceConfig, color::LapceColor, icon::LapceIcons},
    listener::Listener,
    update::ReleaseInfo,
    workspace::LapceWorkspace,
    workspace_data::WorkspaceData,
};

fn branch_indicator(
    git_branch: RwSignal<Option<String>>,
    git_repo_state: RwSignal<GitRepoState>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let has_branch = move || git_branch.with(|b| b.is_some());
    let has_repo_state = move || git_repo_state.with(|s| s.label().is_some());
    stack((
        svg(move || config.get().ui_svg(LapceIcons::GIT_BRANCH)).style(move |s| {
            let config = config.get();
            s.size(16.0, 16.0)
                .color(config.color(LapceColor::EDITOR_FOREGROUND))
        }),
        label(move || git_branch.with(|b| b.clone().unwrap_or_default())).style(
            move |s| {
                let config = config.get();
                s.font_size(config.ui.font_size() as f32)
                    .color(config.color(LapceColor::EDITOR_FOREGROUND))
                    .margin_left(4.0)
            },
        ),
        label(move || {
            git_repo_state
                .with(|s| s.label().map(|l| format!("({})", l)).unwrap_or_default())
        })
        .style(move |s| {
            let config = config.get();
            s.font_size(config.ui.font_size() as f32)
                .color(config.color(LapceColor::LAPCE_WARN))
                .margin_left(6.0)
                .apply_if(!has_repo_state(), |s| s.hide())
        }),
    ))
    .style(move |s| {
        s.items_center()
            .padding_horiz(8.0)
            .apply_if(!has_branch(), |s| s.hide())
    })
}

fn workspace_name(
    workspace: Arc<LapceWorkspace>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let name = workspace.display();
    let has_name = name.is_some();
    label(move || name.clone().unwrap_or_default()).style(move |s| {
        let config = config.get();
        s.font_size(config.ui.font_size() as f32)
            .color(config.color(LapceColor::EDITOR_FOREGROUND))
            .margin_left(10.0)
            .apply_if(!has_name, |s| s.hide())
    })
}

/// Left side of the title bar. On macOS, includes a 75px spacer for the native
/// traffic-light window buttons. On other platforms, shows the Lapce logo and
/// a hamburger menu. The remaining space is a drag area for moving the window.
fn left(
    lapce_command: Listener<LapceCommand>,
    workbench_command: Listener<LapceWorkbenchCommand>,
    window_command: Listener<WindowCommand>,
    current_workspace: Arc<LapceWorkspace>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let is_macos = cfg!(target_os = "macos");
    stack((
        // Reserve space for macOS traffic-light buttons (close/minimize/maximize)
        empty().style(move |s| s.width(75.0).apply_if(!is_macos, |s| s.hide())),
        container(svg(move || config.get().ui_svg(LapceIcons::LOGO)).style(
            move |s| {
                let config = config.get();
                s.size(18.0, 18.0)
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
            window_menu(
                lapce_command,
                workbench_command,
                window_command,
                &current_workspace,
            )
        })
        .style(move |s| {
            s.margin_left(4.0)
                .margin_right(6.0)
                .apply_if(is_macos, |s| s.hide())
        }),
    ))
    .style(move |s| s.height_pct(100.0).items_center())
    .debug_name("Left Side of Top Bar")
}

fn right(
    window_command: Listener<WindowCommand>,
    workbench_command: Listener<LapceWorkbenchCommand>,
    latest_release: ReadSignal<Arc<Option<ReleaseInfo>>>,
    update_in_progress: RwSignal<bool>,
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
                    .entry(MenuItem::new("Open Settings").action(move || {
                        workbench_command.send(LapceWorkbenchCommand::OpenSettings)
                    }))
                    .entry(MenuItem::new("Open Keyboard Shortcuts").action(
                        move || {
                            workbench_command
                                .send(LapceWorkbenchCommand::OpenKeyboardShortcuts)
                        },
                    ))
                    .entry(MenuItem::new("Show Projects").action(move || {
                        workbench_command.send(LapceWorkbenchCommand::ShowProjects)
                    }))
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
                    .entry(MenuItem::new("About SourceDelve").action(move || {
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
        window_controls_view(window_command, window_maximized, config),
    ))
    .style(|s| {
        s.flex_basis(0)
            .flex_grow(1.0)
            .justify_content(Some(JustifyContent::FlexEnd))
    })
    .debug_name("Right of top bar")
}

pub fn title(workspace_data: Rc<WorkspaceData>) -> impl View {
    let lapce_command = workspace_data.common.lapce_command;
    let workbench_command = workspace_data.common.workbench_command;
    let window_command = workspace_data.common.window_common.window_command;
    let current_workspace = workspace_data.workspace.clone();
    let latest_release = workspace_data.common.window_common.latest_release;
    let window_maximized = workspace_data.common.window_common.window_maximized;
    let title_height = workspace_data.layout.title_height;
    let update_in_progress = workspace_data.update_in_progress;
    let git_branch = workspace_data.git.branch;
    let git_repo_state = workspace_data.git.repo_state;
    let config = workspace_data.common.config;
    stack((
        left(
            lapce_command,
            workbench_command,
            window_command,
            current_workspace.clone(),
            config,
        ),
        workspace_name(current_workspace, config),
        drag_window_area(branch_indicator(git_branch, git_repo_state, config))
            .style(|s| {
                s.height_pct(100.0)
                    .items_center()
                    .margin_left(30.0)
                    .flex_basis(0.0)
                    .flex_grow(1.0)
            }),
        right(
            window_command,
            workbench_command,
            latest_release,
            update_in_progress,
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
        s.width_pct(100.0)
            .height(42.0)
            .padding_top(2.0)
            .items_center()
    })
    .debug_name("Title / Top Bar")
}

/// Custom window control buttons (minimize, maximize/restore, close) for platforms
/// using the custom titlebar (non-macOS with custom_titlebar enabled). Hidden on macOS
/// since the native traffic-light buttons handle these functions.
pub fn window_controls_view(
    window_command: Listener<WindowCommand>,
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
                || !config.get_untracked().core.custom_titlebar,
            |s| s.hide(),
        )
    })
}
