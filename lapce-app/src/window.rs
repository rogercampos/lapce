use std::{path::PathBuf, rc::Rc, sync::Arc};

use floem::{
    ViewId,
    action::TimerToken,
    peniko::kurbo::{Point, Size},
    reactive::{
        ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        use_context,
    },
    window::WindowId,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::AppCommand,
    command::{InternalCommand, WindowCommand},
    config::LapceConfig,
    db::LapceDb,
    keypress::EventRef,
    listener::Listener,
    update::ReleaseInfo,
    workspace::LapceWorkspace,
    workspace_data::WorkspaceData,
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

#[derive(Clone)]
pub struct WindowCommonData {
    pub window_command: Listener<WindowCommand>,
    pub window_scale: RwSignal<f64>,
    pub size: RwSignal<Size>,
    pub window_maximized: RwSignal<bool>,
    pub latest_release: ReadSignal<Arc<Option<ReleaseInfo>>>,
    pub ime_allowed: RwSignal<bool>,
    pub cursor_blink_timer: RwSignal<TimerToken>,
    // the value to be update by curosr blinking
    pub hide_cursor: RwSignal<bool>,
    pub app_view_id: RwSignal<ViewId>,
    pub extra_plugin_paths: Arc<Vec<PathBuf>>,
}

/// `WindowData` is the application model for a top-level window.
///
/// A top-level window can be independently moved around and
/// resized using your window manager. Normally Lapce has only one
/// top-level window, but new ones can be created using the "New Window"
/// command.
///
/// Each window has a single workspace.
#[derive(Clone)]
pub struct WindowData {
    pub window_id: WindowId,
    pub scope: Scope,
    pub workspaces: RwSignal<im::Vector<(RwSignal<usize>, Rc<WorkspaceData>)>>,
    /// The index of the active workspace.
    pub active: RwSignal<usize>,
    pub app_command: Listener<AppCommand>,
    pub position: RwSignal<Point>,
    pub root_view_id: RwSignal<ViewId>,
    pub window_scale: RwSignal<f64>,
    pub config: RwSignal<Arc<LapceConfig>>,
    pub ime_enabled: RwSignal<bool>,
    pub common: Rc<WindowCommonData>,
}

impl WindowData {
    pub fn new(
        window_id: WindowId,
        app_view_id: RwSignal<ViewId>,
        info: WindowInfo,
        window_scale: RwSignal<f64>,
        latest_release: ReadSignal<Arc<Option<ReleaseInfo>>>,
        extra_plugin_paths: Arc<Vec<PathBuf>>,
        app_command: Listener<AppCommand>,
    ) -> Self {
        let cx = Scope::new();
        let config =
            LapceConfig::load(&LapceWorkspace::default(), &[], &extra_plugin_paths);
        let config = cx.create_rw_signal(Arc::new(config));
        let root_view_id = cx.create_rw_signal(ViewId::new());

        let workspaces = cx.create_rw_signal(im::Vector::new());
        let active = info.tabs.active_tab;
        let window_command = Listener::new_empty(cx);
        let ime_allowed = cx.create_rw_signal(false);
        let window_maximized = cx.create_rw_signal(false);
        let size = cx.create_rw_signal(Size::ZERO);
        let cursor_blink_timer = cx.create_rw_signal(TimerToken::INVALID);
        let hide_cursor = cx.create_rw_signal(false);

        let common = Rc::new(WindowCommonData {
            window_command,
            window_scale,
            size,
            window_maximized,
            latest_release,
            ime_allowed,
            cursor_blink_timer,
            hide_cursor,
            app_view_id,
            extra_plugin_paths,
        });

        for w in info.tabs.workspaces {
            let workspace =
                Rc::new(WorkspaceData::new(cx, Arc::new(w), common.clone()));
            workspaces.update(|workspaces| {
                workspaces.push_back((cx.create_rw_signal(0), workspace));
            });
        }

        if workspaces.with_untracked(|workspaces| workspaces.is_empty()) {
            let workspace = Rc::new(WorkspaceData::new(
                cx,
                Arc::new(LapceWorkspace::default()),
                common.clone(),
            ));
            workspaces.update(|workspaces| {
                workspaces.push_back((cx.create_rw_signal(0), workspace));
            });
        }

        let active = cx.create_rw_signal(active);
        let position = cx.create_rw_signal(info.pos);

        let window_data = Self {
            window_id,
            scope: cx,
            workspaces,
            active,
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

        {
            cx.create_effect(move |_| {
                let active = active.get();
                let tab = workspaces
                    .with(|tabs| tabs.get(active).map(|(_, tab)| tab.clone()));
                if let Some(tab) = tab {
                    tab.common
                        .internal_command
                        .send(InternalCommand::ResetBlinkCursor);
                }
            })
        }

        window_data
    }

    pub fn reload_config(&self) {
        let config = LapceConfig::load(
            &LapceWorkspace::default(),
            &[],
            &self.common.extra_plugin_paths,
        );
        self.config.set(Arc::new(config));
        let workspaces = self.workspaces.get_untracked();
        for (_, workspace) in workspaces {
            workspace.reload_config();
        }
    }

    pub fn run_window_command(&self, cmd: WindowCommand) {
        match cmd {
            WindowCommand::SetWorkspace { workspace } => {
                let db: Arc<LapceDb> = use_context().unwrap();
                if let Err(err) = db.update_recent_workspace(&workspace) {
                    tracing::error!("{:?}", err);
                }

                let active = self.active.get_untracked();
                self.workspaces.with_untracked(|workspaces| {
                    if !workspaces.is_empty() {
                        let active = workspaces.len().saturating_sub(1).min(active);
                        if let Err(err) =
                            db.insert_workspace_data(workspaces[active].1.clone())
                        {
                            tracing::error!("{:?}", err);
                        }
                    }
                });

                let workspace = Rc::new(WorkspaceData::new(
                    self.scope,
                    Arc::new(workspace),
                    self.common.clone(),
                ));
                self.workspaces.update(|workspaces| {
                    if workspaces.is_empty() {
                        workspaces
                            .push_back((self.scope.create_rw_signal(0), workspace));
                    } else {
                        let active = workspaces.len().saturating_sub(1).min(active);
                        let (_, old_workspace) = workspaces.set(
                            active,
                            (self.scope.create_rw_signal(0), workspace),
                        );
                        old_workspace.proxy.shutdown();
                    }
                })
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
        let active = self.active.get_untracked();
        let workspace = self.workspaces.with_untracked(|workspaces| {
            workspaces
                .get(active)
                .or_else(|| workspaces.last())
                .cloned()
        });
        if let Some((_, workspace)) = workspace {
            workspace.key_down(event)
        } else {
            false
        }
    }

    pub fn info(&self) -> WindowInfo {
        let workspaces: Vec<LapceWorkspace> = self
            .workspaces
            .get_untracked()
            .iter()
            .map(|(_, t)| (*t.workspace).clone())
            .collect();
        WindowInfo {
            size: self.common.size.get_untracked(),
            pos: self.position.get_untracked(),
            maximised: false,
            tabs: TabsInfo {
                active_tab: self.active.get_untracked(),
                workspaces,
            },
        }
    }

    pub fn active_workspace(&self) -> Option<Rc<WorkspaceData>> {
        let workspaces = self.workspaces.get_untracked();
        let active = self
            .active
            .get_untracked()
            .min(workspaces.len().saturating_sub(1));
        workspaces.get(active).map(|(_, tab)| tab.clone())
    }
}
