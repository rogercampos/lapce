use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use floem::{
    action::TimerToken,
    reactive::{RwSignal, Scope},
};
use indexmap::IndexMap;
use lsp_types::{ProgressToken, ShowMessageParams};

use crate::workspace_data::BackgroundTaskInfo;

/// Workspace-level state tracking LSP progress reporting, surfaced tasks, and
/// window/message notifications. Owned by `WorkspaceData` and populated by the
/// `$/progress`, `window/showMessage`, and server-status pipelines in
/// `CoreNotification` handling.
#[derive(Clone)]
pub struct LspProgressState {
    /// In-flight background tasks keyed by app-side task ID, displayed in the
    /// status bar.
    pub background_tasks: RwSignal<IndexMap<u64, BackgroundTaskInfo>>,
    /// Whether the status-bar tasks popup is open.
    pub bg_tasks_popup_visible: RwSignal<bool>,
    /// Maps `(server_name, ProgressToken)` to our app-side task IDs so concurrent
    /// progress streams from multiple LSP servers stay separated.
    pub progress_task_map: RwSignal<HashMap<(String, ProgressToken), u64>>,
    /// Monotonic counter for generating app-side task IDs for LSP progress items.
    pub local_task_id: Arc<std::sync::atomic::AtomicU64>,
    /// Accumulated LSP `window/showMessage` notifications for display.
    pub messages: RwSignal<Vec<(String, ShowMessageParams)>>,
    /// Languages that have had a `ServerStatus` OK and are pending a debounced
    /// refresh of LSP data (semantic tokens, inlay hints, etc.).
    pub pending_server_status_languages: Rc<Cell<HashSet<String>>>,
    /// Timer token for the debounced server status refresh.
    pub pending_server_status_timer: Rc<Cell<TimerToken>>,
}

impl LspProgressState {
    pub fn new(cx: Scope) -> Self {
        Self {
            background_tasks: cx.create_rw_signal(IndexMap::new()),
            bg_tasks_popup_visible: cx.create_rw_signal(false),
            progress_task_map: cx.create_rw_signal(HashMap::new()),
            local_task_id: Arc::new(std::sync::atomic::AtomicU64::new(
                1_000_000_000,
            )),
            messages: cx.create_rw_signal(Vec::new()),
            pending_server_status_languages: Rc::new(Cell::new(HashSet::new())),
            pending_server_status_timer: Rc::new(Cell::new(TimerToken::INVALID)),
        }
    }
}
