use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use indexmap::IndexMap;

use crate::proxy::SearchMatch;

pub type BackgroundTaskId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Queued {
        name: String,
    },
    Started {
        name: String,
    },
    Progress {
        message: Option<String>,
        percentage: Option<u32>,
    },
    Finished,
}

use crossbeam_channel::{Receiver, Sender};
use lsp_types::{
    CancelParams, CompletionResponse, LogMessageParams, ProgressParams,
    PublishDiagnosticsParams, ShowMessageParams, SignatureHelp,
};
use serde::{Deserialize, Serialize};

use crate::{
    RequestId, RpcMessage, file::PathObject, plugin::PluginId, project::ProjectInfo,
};

/// Internal channel message type for proxy-to-UI communication.
/// CoreNotification is boxed because some variants (like CompletionResponse)
/// are large, and clippy warns about enum variant size disparity.
pub enum CoreRpc {
    Request(RequestId, CoreRequest),
    Notification(Box<CoreNotification>),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GitRepoState {
    #[default]
    Normal,
    Rebasing,
    Merging,
    CherryPicking,
    Reverting,
}

impl GitRepoState {
    pub fn label(&self) -> Option<&'static str> {
        match self {
            GitRepoState::Normal => None,
            GitRepoState::Rebasing => Some("Rebasing"),
            GitRepoState::Merging => Some("Merging"),
            GitRepoState::CherryPicking => Some("Cherry-Picking"),
            GitRepoState::Reverting => Some("Reverting"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChanged {
    Change(String),
    Delete,
}

/// Messages sent from the proxy back to the UI. These are all notifications
/// (fire-and-forget) because the proxy never needs the UI to respond.
/// Includes LSP results (completions, diagnostics), plugin lifecycle events,
/// and file change notifications from the file watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "method", content = "params")]
pub enum CoreNotification {
    OpenFileChanged {
        path: PathBuf,
        content: FileChanged,
    },
    CompletionResponse {
        request_id: usize,
        input: String,
        resp: CompletionResponse,
        plugin_id: PluginId,
    },
    SignatureHelpResponse {
        request_id: usize,
        resp: SignatureHelp,
        plugin_id: PluginId,
    },
    OpenPaths {
        paths: Vec<PathObject>,
    },
    WorkspaceFileChange,
    GitHeadChanged {
        head: Option<String>,
        repo_state: GitRepoState,
    },
    GitFileStatusChanged {
        statuses: HashMap<PathBuf, GitFileStatus>,
    },
    /// Incremental git status update: carries only the changed entries.
    /// `None` values mean the file reverted to clean (remove from map).
    GitFileStatusDiff {
        changes: HashMap<PathBuf, Option<GitFileStatus>>,
    },
    PublishDiagnostics {
        diagnostics: PublishDiagnosticsParams,
    },
    ServerStatus {
        params: ServerStatusParams,
    },
    WorkDoneProgress {
        progress: ProgressParams,
        server_name: String,
    },
    ShowMessage {
        title: String,
        message: ShowMessageParams,
    },
    LogMessage {
        message: LogMessageParams,
        target: String,
    },
    LspCancel {
        params: CancelParams,
    },
    Log {
        level: LogLevel,
        message: String,
        target: Option<String>,
    },
    ProjectsDetected {
        projects: Vec<ProjectInfo>,
    },
    BackgroundTaskUpdate {
        task_id: BackgroundTaskId,
        status: BackgroundTaskStatus,
    },
    /// Incremental batch of file paths discovered during workspace indexing.
    GetFilesDiff {
        paths: Vec<PathBuf>,
    },
    /// Signals that workspace file indexing has completed.
    GetFilesDone,
    /// Incremental batch of global search results, sent as files are matched.
    GlobalSearchDiffMatches {
        search_id: u64,
        matches: IndexMap<PathBuf, Vec<SearchMatch>>,
    },
    /// Signals that the current global search has completed.
    GlobalSearchDone {
        search_id: u64,
    },
    /// Signals that a global replace operation has finished, listing the files
    /// that were modified on disk so the UI can reload open documents.
    GlobalReplaceDone {
        modified_files: Vec<PathBuf>,
    },
}

/// Currently empty -- the proxy never makes requests to the UI that require
/// a response. Kept as a placeholder for future bidirectional request patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "method", content = "params")]
pub enum CoreResponse {}

pub type CoreMessage = RpcMessage<CoreRequest, CoreNotification, CoreResponse>;

pub trait CoreHandler {
    fn handle_notification(&mut self, rpc: CoreNotification);
    fn handle_request(&mut self, id: RequestId, rpc: CoreRequest);
}

#[derive(Clone)]
pub struct CoreRpcHandler {
    tx: Sender<CoreRpc>,
    rx: Receiver<CoreRpc>,
    bg_task_id: Arc<AtomicU64>,
}

impl CoreRpcHandler {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            tx,
            rx,
            bg_task_id: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn mainloop<H>(&self, handler: &mut H)
    where
        H: CoreHandler,
    {
        for msg in &self.rx {
            match msg {
                CoreRpc::Request(id, rpc) => {
                    handler.handle_request(id, rpc);
                }
                CoreRpc::Notification(rpc) => {
                    handler.handle_notification(*rpc);
                }
                CoreRpc::Shutdown => {
                    return;
                }
            }
        }
    }

    pub fn rx(&self) -> &Receiver<CoreRpc> {
        &self.rx
    }

    pub fn shutdown(&self) {
        if let Err(err) = self.tx.send(CoreRpc::Shutdown) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn notification(&self, notification: CoreNotification) {
        if let Err(err) = self.tx.send(CoreRpc::Notification(Box::new(notification)))
        {
            tracing::error!("{:?}", err);
        }
    }

    pub fn workspace_file_change(&self) {
        self.notification(CoreNotification::WorkspaceFileChange);
    }

    pub fn git_head_changed(&self, head: Option<String>, repo_state: GitRepoState) {
        self.notification(CoreNotification::GitHeadChanged { head, repo_state });
    }

    pub fn git_file_status_changed(
        &self,
        statuses: HashMap<PathBuf, GitFileStatus>,
    ) {
        self.notification(CoreNotification::GitFileStatusChanged { statuses });
    }

    pub fn git_file_status_diff(
        &self,
        changes: HashMap<PathBuf, Option<GitFileStatus>>,
    ) {
        self.notification(CoreNotification::GitFileStatusDiff { changes });
    }

    pub fn open_file_changed(&self, path: PathBuf, content: FileChanged) {
        self.notification(CoreNotification::OpenFileChanged { path, content });
    }

    pub fn completion_response(
        &self,
        request_id: usize,
        input: String,
        resp: CompletionResponse,
        plugin_id: PluginId,
    ) {
        self.notification(CoreNotification::CompletionResponse {
            request_id,
            input,
            resp,
            plugin_id,
        });
    }

    pub fn signature_help_response(
        &self,
        request_id: usize,
        resp: SignatureHelp,
        plugin_id: PluginId,
    ) {
        self.notification(CoreNotification::SignatureHelpResponse {
            request_id,
            resp,
            plugin_id,
        });
    }

    pub fn log(&self, level: LogLevel, message: String, target: Option<String>) {
        self.notification(CoreNotification::Log {
            level,
            message,
            target,
        });
    }

    pub fn publish_diagnostics(&self, diagnostics: PublishDiagnosticsParams) {
        self.notification(CoreNotification::PublishDiagnostics { diagnostics });
    }

    pub fn server_status(&self, params: ServerStatusParams) {
        self.notification(CoreNotification::ServerStatus { params });
    }

    pub fn work_done_progress(&self, progress: ProgressParams, server_name: String) {
        self.notification(CoreNotification::WorkDoneProgress {
            progress,
            server_name,
        });
    }

    pub fn show_message(&self, title: String, message: ShowMessageParams) {
        self.notification(CoreNotification::ShowMessage { title, message });
    }

    pub fn log_message(&self, message: LogMessageParams, target: String) {
        self.notification(CoreNotification::LogMessage { message, target });
    }

    pub fn cancel(&self, params: CancelParams) {
        self.notification(CoreNotification::LspCancel { params });
    }

    pub fn projects_detected(&self, projects: Vec<ProjectInfo>) {
        self.notification(CoreNotification::ProjectsDetected { projects });
    }

    pub fn next_background_task_id(&self) -> BackgroundTaskId {
        self.bg_task_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn background_task_queued(&self, task_id: BackgroundTaskId, name: String) {
        self.notification(CoreNotification::BackgroundTaskUpdate {
            task_id,
            status: BackgroundTaskStatus::Queued { name },
        });
    }

    pub fn background_task_started(&self, task_id: BackgroundTaskId, name: String) {
        self.notification(CoreNotification::BackgroundTaskUpdate {
            task_id,
            status: BackgroundTaskStatus::Started { name },
        });
    }

    pub fn background_task_progress(
        &self,
        task_id: BackgroundTaskId,
        message: Option<String>,
        percentage: Option<u32>,
    ) {
        self.notification(CoreNotification::BackgroundTaskUpdate {
            task_id,
            status: BackgroundTaskStatus::Progress {
                message,
                percentage,
            },
        });
    }

    pub fn background_task_finished(&self, task_id: BackgroundTaskId) {
        self.notification(CoreNotification::BackgroundTaskUpdate {
            task_id,
            status: BackgroundTaskStatus::Finished,
        });
    }

    pub fn get_files_diff(&self, paths: Vec<PathBuf>) {
        self.notification(CoreNotification::GetFilesDiff { paths });
    }

    pub fn get_files_done(&self) {
        self.notification(CoreNotification::GetFilesDone);
    }

    pub fn global_search_diff_matches(
        &self,
        search_id: u64,
        matches: IndexMap<PathBuf, Vec<SearchMatch>>,
    ) {
        self.notification(CoreNotification::GlobalSearchDiffMatches {
            search_id,
            matches,
        });
    }

    pub fn global_search_done(&self, search_id: u64) {
        self.notification(CoreNotification::GlobalSearchDone { search_id });
    }

    pub fn global_replace_done(&self, modified_files: Vec<PathBuf>) {
        self.notification(CoreNotification::GlobalReplaceDone { modified_files });
    }
}

impl Default for CoreRpcHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Info = 0,
    Warn = 1,
    Error = 2,
    Debug = 3,
    Trace = 4,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerStatusParams {
    health: String,
    pub message: Option<String>,
}

impl ServerStatusParams {
    pub fn is_ok(&self) -> bool {
        self.health.as_str() == "ok"
    }
}
