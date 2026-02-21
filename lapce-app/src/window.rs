use std::{rc::Rc, sync::Arc};

use floem::{
    ViewId,
    action::TimerToken,
    peniko::kurbo::{Point, Size},
    reactive::{ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, use_context},
    window::WindowId,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::AppCommand, command::WindowCommand, config::LapceConfig, db::LapceDb,
    keypress::EventRef, listener::Listener, update::ReleaseInfo,
    workspace::LapceWorkspace, workspace_data::WorkspaceData,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabsInfo {
    pub active_tab: usize,
    pub workspaces: Vec<LapceWorkspace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub size: Size,
    pub pos: Point,
    pub maximised: bool,
    pub tabs: TabsInfo,
}

impl WindowInfo {
    /// Create a WindowInfo for a single workspace.
    pub fn for_workspace(workspace: LapceWorkspace, size: Size, pos: Point) -> Self {
        Self {
            size,
            pos,
            maximised: false,
            tabs: TabsInfo {
                active_tab: 0,
                workspaces: vec![workspace],
            },
        }
    }
}

#[derive(Clone)]
pub struct WindowCommonData {
    pub window_id: WindowId,
    pub window_command: Listener<WindowCommand>,
    pub window_scale: RwSignal<f64>,
    pub size: RwSignal<Size>,
    pub window_maximized: RwSignal<bool>,
    pub latest_release: ReadSignal<Arc<Option<ReleaseInfo>>>,
    pub ime_allowed: RwSignal<bool>,
    pub cursor_blink_timer: RwSignal<TimerToken>,
    // the value to be updated by cursor blinking
    pub hide_cursor: RwSignal<bool>,
    pub app_view_id: RwSignal<ViewId>,
}

/// `WindowData` is the application model for a top-level window.
///
/// A top-level window can be independently moved around and
/// resized using your window manager. Each window contains exactly
/// one workspace. Opening a new workspace always creates a new window.
/// Closing the window closes the workspace.
#[derive(Clone)]
pub struct WindowData {
    pub window_id: WindowId,
    pub scope: Scope,
    pub workspace: Rc<WorkspaceData>,
    pub app_command: Listener<AppCommand>,
    pub position: RwSignal<Point>,
    pub root_view_id: RwSignal<ViewId>,
    pub window_scale: RwSignal<f64>,
    pub config: RwSignal<Arc<LapceConfig>>,
    pub ime_enabled: RwSignal<bool>,
    pub common: Rc<WindowCommonData>,
}

impl WindowData {
    /// Create a new window from serialized WindowInfo. This creates a Scope that owns all
    /// the reactive signals for this window's lifetime. The first workspace listed in the info
    /// is instantiated as a full WorkspaceData. If no workspaces are provided, a default
    /// empty workspace is created.
    pub fn new(
        window_id: WindowId,
        app_view_id: RwSignal<ViewId>,
        info: WindowInfo,
        window_scale: RwSignal<f64>,
        latest_release: ReadSignal<Arc<Option<ReleaseInfo>>>,
        app_command: Listener<AppCommand>,
    ) -> Self {
        let cx = Scope::new();
        let config = LapceConfig::load(&LapceWorkspace::default());
        let config = cx.create_rw_signal(Arc::new(config));
        let root_view_id = cx.create_rw_signal(ViewId::new());

        let window_command = Listener::new_empty(cx);
        let ime_allowed = cx.create_rw_signal(false);
        let window_maximized = cx.create_rw_signal(false);
        let size = cx.create_rw_signal(Size::ZERO);
        let cursor_blink_timer = cx.create_rw_signal(TimerToken::INVALID);
        let hide_cursor = cx.create_rw_signal(false);

        let common = Rc::new(WindowCommonData {
            window_id,
            window_command,
            window_scale,
            size,
            window_maximized,
            latest_release,
            ime_allowed,
            cursor_blink_timer,
            hide_cursor,
            app_view_id,
        });

        // Use the first workspace from the info, or a default empty workspace
        let ws = info.tabs.workspaces.into_iter().next().unwrap_or_default();
        let workspace =
            Rc::new(WorkspaceData::new(cx, Arc::new(ws), common.clone()));

        let position = cx.create_rw_signal(info.pos);

        let window_data = Self {
            window_id,
            scope: cx,
            workspace,
            position,
            root_view_id,
            window_scale,
            app_command,
            config,
            ime_enabled: cx.create_rw_signal(false),
            common,
        };

        {
            let window_data = window_data.clone();
            window_data.common.window_command.listen(move |cmd| {
                window_data.run_window_command(cmd);
            });
        }

        window_data
    }

    pub fn reload_config(&self) {
        let config = LapceConfig::load(&LapceWorkspace::default());
        self.config.set(Arc::new(config));
        self.workspace.reload_config();
    }

    /// Handle window-level commands. SetWorkspace opens the workspace in a new window.
    /// ReloadWindow replaces the current workspace in-place.
    /// Every window command triggers a SaveApp afterward to keep the persisted state in sync.
    pub fn run_window_command(&self, cmd: WindowCommand) {
        match cmd {
            WindowCommand::SetWorkspace { workspace } => {
                let db: Arc<LapceDb> =
                    use_context().expect("LapceDb must be provided as context");
                if let Err(err) = db.update_recent_workspace(&workspace) {
                    tracing::error!("{:?}", err);
                }

                // Open the workspace in a new window
                self.app_command.send(AppCommand::NewWindow {
                    folder: workspace.path,
                });

                // If the current window has no folder (empty workspace), close it
                if self.workspace.workspace.path.is_none() {
                    self.app_command
                        .send(AppCommand::CloseWindow(self.window_id));
                }
            }
            WindowCommand::ReloadWindow => {
                // Reload replaces the current workspace in-place within this window
                let db: Arc<LapceDb> =
                    use_context().expect("LapceDb must be provided as context");
                if let Err(err) = db.insert_workspace_data(self.workspace.clone()) {
                    tracing::error!("{:?}", err);
                }
                // We need to create a new workspace and update the window.
                // Since WindowData.workspace is an Rc (not a signal), we close
                // and re-open this window to reload it.
                let workspace = (*self.workspace.workspace).clone();
                self.app_command.send(AppCommand::NewWindow {
                    folder: workspace.path,
                });
                self.app_command
                    .send(AppCommand::CloseWindow(self.window_id));
            }
            WindowCommand::NewWindow => {
                self.app_command
                    .send(AppCommand::NewWindow { folder: None });
            }
            WindowCommand::CloseWindow => {
                self.app_command
                    .send(AppCommand::CloseWindow(self.window_id));
            }
        }
        self.app_command.send(AppCommand::SaveApp);
    }

    pub fn key_down<'a>(&self, event: impl Into<EventRef<'a>> + Copy) -> bool {
        self.workspace.key_down(event)
    }

    pub fn info(&self) -> WindowInfo {
        WindowInfo {
            size: self.common.size.get_untracked(),
            pos: self.position.get_untracked(),
            maximised: false,
            tabs: TabsInfo {
                active_tab: 0,
                workspaces: vec![(*self.workspace.workspace).clone()],
            },
        }
    }

    pub fn active_workspace(&self) -> Option<Rc<WorkspaceData>> {
        Some(self.workspace.clone())
    }
}
