use std::{collections::HashMap, path::PathBuf};

use floem::reactive::{RwSignal, Scope};
use lapce_rpc::core::{GitFileStatus, GitRepoState};

/// Workspace-level git state. Populated by `CoreNotification` messages from the
/// proxy (branch changes, repo state transitions, file status updates) and read
/// by the title bar, file explorer decorations, and status components.
#[derive(Clone, Copy)]
pub struct GitState {
    pub branch: RwSignal<Option<String>>,
    pub repo_state: RwSignal<GitRepoState>,
    pub file_statuses: RwSignal<HashMap<PathBuf, GitFileStatus>>,
}

impl GitState {
    pub fn new(cx: Scope) -> Self {
        Self {
            branch: cx.create_rw_signal(None),
            repo_state: cx.create_rw_signal(GitRepoState::Normal),
            file_statuses: cx.create_rw_signal(HashMap::new()),
        }
    }
}
