use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crossbeam_channel::{Receiver, Sender};
use indexmap::IndexMap;
use lapce_xi_rope::RopeDelta;
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CodeAction, CodeActionResponse,
    CodeLens, CompletionItem, Diagnostic, FoldingRange, GotoDefinitionResponse,
    Hover, InlayHint, InlineCompletionResponse, InlineCompletionTriggerKind,
    Location, Position, PrepareRenameResponse, SelectionRange, TextDocumentItem,
    TextEdit, WorkspaceEdit,
    request::{GotoImplementationResponse, GotoTypeDefinitionResponse},
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{
    RequestId, RpcError, RpcMessage,
    buffer::BufferId,
    file::{FileNodeItem, PathObject},
    file_line::FileLine,
    plugin::PluginId,
    style::SemanticStyles,
};

/// Internal message type used on the crossbeam channel between the UI thread
/// and the proxy handler. This is the in-process representation -- not
/// serialized. ProxyRequest/ProxyNotification are the serializable wire formats
/// used when communicating over stdio.
#[allow(clippy::large_enum_variant)]
pub enum ProxyRpc {
    Request(RequestId, ProxyRequest),
    Notification(ProxyNotification),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchMatch {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub line_content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolInformationEntry {
    pub name: String,
    pub kind: lsp_types::SymbolKind,
    pub location: lsp_types::Location,
    pub container_name: Option<String>,
}

/// Messages from the UI to the proxy that expect a response. Serialized as
/// tagged JSON: `{"method": "new_buffer", "params": {...}, "id": 42}`.
/// Each variant maps to an LSP or file-system operation handled by the proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "method", content = "params")]
pub enum ProxyRequest {
    NewBuffer {
        buffer_id: BufferId,
        path: PathBuf,
    },
    BufferHead {
        path: PathBuf,
    },
    GlobalSearch {
        pattern: String,
        case_sensitive: bool,
        whole_word: bool,
        is_regex: bool,
        /// When set, stop searching after this many total matches.
        /// Used by the search modal for fast preview results.
        max_results: Option<usize>,
        /// Unique ID for this search request. Used to route streaming
        /// notifications back to the correct `GlobalSearchData` instance.
        search_id: u64,
        /// When set, restrict the search to this subfolder (relative to
        /// the workspace root). `None` means search the entire workspace.
        search_path: Option<PathBuf>,
    },
    GlobalReplace {
        pattern: String,
        replacement: String,
        case_sensitive: bool,
        whole_word: bool,
        is_regex: bool,
    },
    CompletionResolve {
        plugin_id: PluginId,
        completion_item: Box<CompletionItem>,
    },
    CodeActionResolve {
        plugin_id: PluginId,
        action_item: Box<CodeAction>,
    },
    GetHover {
        request_id: usize,
        path: PathBuf,
        position: Position,
    },
    GetSignature {
        buffer_id: BufferId,
        position: Position,
    },
    GetSelectionRange {
        path: PathBuf,
        positions: Vec<Position>,
    },
    GetReferences {
        path: PathBuf,
        position: Position,
    },
    GotoImplementation {
        path: PathBuf,
        position: Position,
    },
    GetDefinition {
        request_id: usize,
        path: PathBuf,
        position: Position,
    },
    ShowCallHierarchy {
        path: PathBuf,
        position: Position,
    },
    CallHierarchyIncoming {
        path: PathBuf,
        call_hierarchy_item: CallHierarchyItem,
    },
    GetTypeDefinition {
        request_id: usize,
        path: PathBuf,
        position: Position,
    },
    GetInlayHints {
        path: PathBuf,
    },
    GetDocumentDiagnostics {
        path: PathBuf,
    },
    GetInlineCompletions {
        path: PathBuf,
        position: Position,
        trigger_kind: InlineCompletionTriggerKind,
    },
    GetSemanticTokens {
        path: PathBuf,
    },
    LspFoldingRange {
        path: PathBuf,
    },
    PrepareRename {
        path: PathBuf,
        position: Position,
    },
    Rename {
        path: PathBuf,
        position: Position,
        new_name: String,
    },
    GetCodeActions {
        path: PathBuf,
        position: Position,
        diagnostics: Vec<Diagnostic>,
    },
    GetCodeLens {
        path: PathBuf,
    },
    GetCodeLensResolve {
        code_lens: CodeLens,
        path: PathBuf,
    },
    GetDocumentFormatting {
        path: PathBuf,
    },
    GetOpenFilesContent {},
    GetFiles {},
    ReadDir {
        path: PathBuf,
    },
    Save {
        rev: u64,
        path: PathBuf,
        /// Whether to create the parent directories if they do not exist.
        create_parents: bool,
    },
    SaveBufferAs {
        buffer_id: BufferId,
        path: PathBuf,
        rev: u64,
        content: String,
        /// Whether to create the parent directories if they do not exist.
        create_parents: bool,
    },
    CreateFile {
        path: PathBuf,
    },
    CreateDirectory {
        path: PathBuf,
    },
    TrashPath {
        path: PathBuf,
    },
    DuplicatePath {
        existing_path: PathBuf,
        new_path: PathBuf,
    },
    RenamePath {
        from: PathBuf,
        to: PathBuf,
    },
    TestCreateAtPath {
        path: PathBuf,
    },
    ReferencesResolve {
        items: Vec<Location>,
    },
    GetWorkspaceSymbols {
        path: PathBuf,
        query: String,
    },
    /// List all directories in the workspace (recursively) using a fast
    /// filesystem walk.  Used by the folder picker for search/filter.
    ListAllFolders {},
}

/// Fire-and-forget messages from the UI to the proxy (no response expected).
/// These include text edits (Update), plugin management, and initialization.
/// Completions and signature help are notifications rather than requests because
/// the proxy sends results back as CoreNotifications (allowing multiple
/// responses from multiple language servers).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "method", content = "params")]
pub enum ProxyNotification {
    Initialize {
        workspace: Option<PathBuf>,
        window_id: usize,
        tab_id: usize,
        ruby_lsp_exclude_gems: bool,
        ruby_lsp_excluded_patterns: Vec<String>,
        excluded_directories: Vec<String>,
    },
    OpenFileChanged {
        path: PathBuf,
    },
    OpenPaths {
        paths: Vec<PathObject>,
    },
    UpdateExcludedDirectories {
        excluded_directories: Vec<String>,
    },
    Shutdown {},
    Completion {
        request_id: usize,
        path: PathBuf,
        input: String,
        position: Position,
    },
    SignatureHelp {
        request_id: usize,
        path: PathBuf,
        position: Position,
    },
    Update {
        path: PathBuf,
        delta: RopeDelta,
        rev: u64,
    },
    LspCancel {
        id: i32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "method", content = "params")]
pub enum ProxyResponse {
    NewBufferResponse {
        content: String,
        read_only: bool,
    },
    BufferHeadResponse {
        version: String,
        content: String,
    },
    ReadDirResponse {
        items: Vec<FileNodeItem>,
    },
    CompletionResolveResponse {
        item: Box<CompletionItem>,
    },
    CodeActionResolveResponse {
        item: Box<CodeAction>,
    },
    HoverResponse {
        request_id: usize,
        hover: Hover,
    },
    GetDefinitionResponse {
        request_id: usize,
        definition: GotoDefinitionResponse,
    },
    ShowCallHierarchyResponse {
        items: Option<Vec<CallHierarchyItem>>,
    },
    CallHierarchyIncomingResponse {
        items: Option<Vec<CallHierarchyIncomingCall>>,
    },
    GetTypeDefinition {
        request_id: usize,
        definition: GotoTypeDefinitionResponse,
    },
    GetReferencesResponse {
        references: Vec<Location>,
    },
    GetCodeActionsResponse {
        plugin_id: PluginId,
        resp: CodeActionResponse,
    },
    LspFoldingRangeResponse {
        plugin_id: PluginId,
        resp: Option<Vec<FoldingRange>>,
    },
    GetCodeLensResponse {
        plugin_id: PluginId,
        resp: Option<Vec<CodeLens>>,
    },
    GetCodeLensResolveResponse {
        plugin_id: PluginId,
        resp: CodeLens,
    },
    GotoImplementationResponse {
        plugin_id: PluginId,
        resp: Option<GotoImplementationResponse>,
    },
    GetFilesResponse {
        items: Vec<PathBuf>,
    },
    GetDocumentFormatting {
        edits: Vec<TextEdit>,
    },
    GetSelectionRange {
        ranges: Vec<SelectionRange>,
    },
    GetInlayHints {
        hints: Vec<InlayHint>,
    },
    GetDocumentDiagnosticsResponse {
        diagnostics: Vec<Diagnostic>,
    },
    GetInlineCompletions {
        completions: InlineCompletionResponse,
    },
    GetSemanticTokens {
        styles: SemanticStyles,
    },
    PrepareRename {
        resp: PrepareRenameResponse,
    },
    Rename {
        edit: WorkspaceEdit,
    },
    GetOpenFilesContentResponse {
        items: Vec<TextDocumentItem>,
    },
    GlobalSearchResponse {
        matches: IndexMap<PathBuf, Vec<SearchMatch>>,
    },
    GlobalReplaceResponse {
        modified_count: usize,
    },
    CreatePathResponse {
        path: PathBuf,
    },
    Success {},
    SaveResponse {},
    ReferencesResolveResponse {
        items: Vec<FileLine>,
    },
    GetWorkspaceSymbolsResponse {
        symbols: Vec<SymbolInformationEntry>,
    },
    ListAllFoldersResponse {
        folders: Vec<PathBuf>,
    },
}

pub type ProxyMessage = RpcMessage<ProxyRequest, ProxyNotification, ProxyResponse>;

pub trait ProxyCallback: Send + FnOnce(Result<ProxyResponse, RpcError>) {}

impl<F: Send + FnOnce(Result<ProxyResponse, RpcError>)> ProxyCallback for F {}

/// Two ways to handle a proxy response: a one-shot callback (for async requests
/// that process the result on the UI thread) or a channel (for synchronous
/// blocking requests). The callback variant is used by the vast majority of
/// requests; the channel variant is only used by `get_open_files_content`.
enum ResponseHandler {
    Callback(Box<dyn ProxyCallback>),
    Chan(Sender<Result<ProxyResponse, RpcError>>),
}

impl ResponseHandler {
    fn invoke(self, result: Result<ProxyResponse, RpcError>) {
        match self {
            ResponseHandler::Callback(f) => f(result),
            ResponseHandler::Chan(tx) => {
                if let Err(err) = tx.send(result) {
                    tracing::error!("{:?}", err);
                }
            }
        }
    }
}

pub trait ProxyHandler {
    fn handle_notification(&mut self, rpc: ProxyNotification);
    fn handle_request(&mut self, id: RequestId, rpc: ProxyRequest);
}

/// The UI-side handle for communicating with the proxy process. Cloneable
/// and thread-safe. Sends requests/notifications via the crossbeam channel
/// and tracks pending requests in a mutex-protected map keyed by request ID.
/// When a response arrives, `handle_response` looks up and invokes the
/// corresponding callback or unblocks the waiting channel.
#[derive(Clone)]
pub struct ProxyRpcHandler {
    tx: Sender<ProxyRpc>,
    rx: Receiver<ProxyRpc>,
    id: Arc<AtomicU64>,
    pending: Arc<Mutex<HashMap<u64, ResponseHandler>>>,
}

impl ProxyRpcHandler {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            tx,
            rx,
            id: Arc::new(AtomicU64::new(0)),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn rx(&self) -> &Receiver<ProxyRpc> {
        &self.rx
    }

    pub fn mainloop<H>(&self, handler: &mut H)
    where
        H: ProxyHandler,
    {
        use ProxyRpc::*;
        for msg in &self.rx {
            match msg {
                Request(id, request) => {
                    handler.handle_request(id, request);
                }
                Notification(notification) => {
                    handler.handle_notification(notification);
                }
                Shutdown => {
                    return;
                }
            }
        }
    }

    fn request_common(&self, request: ProxyRequest, rh: ResponseHandler) {
        let id = self.id.fetch_add(1, Ordering::Relaxed);

        self.pending.lock().insert(id, rh);

        if let Err(err) = self.tx.send(ProxyRpc::Request(id, request)) {
            tracing::error!("{:?}", err);
        }
    }

    fn request(&self, request: ProxyRequest) -> Result<ProxyResponse, RpcError> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.request_common(request, ResponseHandler::Chan(tx));
        rx.recv().unwrap_or_else(|_| Err(RpcError::new("io error")))
    }

    pub fn request_async(
        &self,
        request: ProxyRequest,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_common(request, ResponseHandler::Callback(Box::new(f)))
    }

    pub fn handle_response(
        &self,
        id: RequestId,
        result: Result<ProxyResponse, RpcError>,
    ) {
        let handler = { self.pending.lock().remove(&id) };
        if let Some(handler) = handler {
            handler.invoke(result);
        }
    }

    pub fn notification(&self, notification: ProxyNotification) {
        if let Err(err) = self.tx.send(ProxyRpc::Notification(notification)) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn lsp_cancel(&self, id: i32) {
        self.notification(ProxyNotification::LspCancel { id });
    }

    pub fn shutdown(&self) {
        self.notification(ProxyNotification::Shutdown {});
        if let Err(err) = self.tx.send(ProxyRpc::Shutdown) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn initialize(
        &self,
        workspace: Option<PathBuf>,
        window_id: usize,
        tab_id: usize,
        ruby_lsp_exclude_gems: bool,
        ruby_lsp_excluded_patterns: Vec<String>,
        excluded_directories: Vec<String>,
    ) {
        self.notification(ProxyNotification::Initialize {
            workspace,
            window_id,
            tab_id,
            ruby_lsp_exclude_gems,
            ruby_lsp_excluded_patterns,
            excluded_directories,
        });
    }

    pub fn completion(
        &self,
        request_id: usize,
        path: PathBuf,
        input: String,
        position: Position,
    ) {
        self.notification(ProxyNotification::Completion {
            request_id,
            path,
            input,
            position,
        });
    }

    pub fn signature_help(
        &self,
        request_id: usize,
        path: PathBuf,
        position: Position,
    ) {
        self.notification(ProxyNotification::SignatureHelp {
            request_id,
            path,
            position,
        });
    }

    pub fn new_buffer(
        &self,
        buffer_id: BufferId,
        path: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::NewBuffer { buffer_id, path }, f);
    }

    pub fn get_buffer_head(&self, path: PathBuf, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::BufferHead { path }, f);
    }

    pub fn create_file(&self, path: PathBuf, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::CreateFile { path }, f);
    }

    pub fn create_directory(&self, path: PathBuf, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::CreateDirectory { path }, f);
    }

    pub fn trash_path(&self, path: PathBuf, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::TrashPath { path }, f);
    }

    pub fn duplicate_path(
        &self,
        existing_path: PathBuf,
        new_path: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::DuplicatePath {
                existing_path,
                new_path,
            },
            f,
        );
    }

    pub fn rename_path(
        &self,
        from: PathBuf,
        to: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::RenamePath { from, to }, f);
    }

    pub fn test_create_at_path(
        &self,
        path: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::TestCreateAtPath { path }, f);
    }

    pub fn save_buffer_as(
        &self,
        buffer_id: BufferId,
        path: PathBuf,
        rev: u64,
        content: String,
        create_parents: bool,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::SaveBufferAs {
                buffer_id,
                path,
                rev,
                content,
                create_parents,
            },
            f,
        );
    }

    pub fn global_search(
        &self,
        pattern: String,
        case_sensitive: bool,
        whole_word: bool,
        is_regex: bool,
        max_results: Option<usize>,
        search_id: u64,
        search_path: Option<PathBuf>,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::GlobalSearch {
                pattern,
                case_sensitive,
                whole_word,
                is_regex,
                max_results,
                search_id,
                search_path,
            },
            f,
        );
    }

    pub fn global_replace(
        &self,
        pattern: String,
        replacement: String,
        case_sensitive: bool,
        whole_word: bool,
        is_regex: bool,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::GlobalReplace {
                pattern,
                replacement,
                case_sensitive,
                whole_word,
                is_regex,
            },
            f,
        );
    }

    pub fn save(
        &self,
        rev: u64,
        path: PathBuf,
        create_parents: bool,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::Save {
                rev,
                path,
                create_parents,
            },
            f,
        );
    }

    pub fn get_files(&self, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::GetFiles {}, f);
    }

    pub fn get_open_files_content(&self) -> Result<ProxyResponse, RpcError> {
        self.request(ProxyRequest::GetOpenFilesContent {})
    }

    pub fn read_dir(&self, path: PathBuf, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::ReadDir { path }, f);
    }

    pub fn completion_resolve(
        &self,
        plugin_id: PluginId,
        completion_item: CompletionItem,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::CompletionResolve {
                plugin_id,
                completion_item: Box::new(completion_item),
            },
            f,
        );
    }

    pub fn code_action_resolve(
        &self,
        action_item: CodeAction,
        plugin_id: PluginId,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::CodeActionResolve {
                action_item: Box::new(action_item),
                plugin_id,
            },
            f,
        );
    }

    pub fn get_hover(
        &self,
        request_id: usize,
        path: PathBuf,
        position: Position,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::GetHover {
                request_id,
                path,
                position,
            },
            f,
        );
    }

    pub fn get_definition(
        &self,
        request_id: usize,
        path: PathBuf,
        position: Position,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::GetDefinition {
                request_id,
                path,
                position,
            },
            f,
        );
    }

    pub fn show_call_hierarchy(
        &self,
        path: PathBuf,
        position: Position,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::ShowCallHierarchy { path, position }, f);
    }

    pub fn call_hierarchy_incoming(
        &self,
        path: PathBuf,
        call_hierarchy_item: CallHierarchyItem,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::CallHierarchyIncoming {
                path,
                call_hierarchy_item,
            },
            f,
        );
    }

    pub fn get_type_definition(
        &self,
        request_id: usize,
        path: PathBuf,
        position: Position,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::GetTypeDefinition {
                request_id,
                path,
                position,
            },
            f,
        );
    }

    pub fn get_lsp_folding_range(
        &self,
        path: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::LspFoldingRange { path }, f);
    }

    pub fn get_references(
        &self,
        path: PathBuf,
        position: Position,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::GetReferences { path, position }, f);
    }

    pub fn references_resolve(
        &self,
        items: Vec<Location>,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::ReferencesResolve { items }, f);
    }

    pub fn go_to_implementation(
        &self,
        path: PathBuf,
        position: Position,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::GotoImplementation { path, position }, f);
    }

    pub fn get_code_actions(
        &self,
        path: PathBuf,
        position: Position,
        diagnostics: Vec<Diagnostic>,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::GetCodeActions {
                path,
                position,
                diagnostics,
            },
            f,
        );
    }

    pub fn get_code_lens(&self, path: PathBuf, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::GetCodeLens { path }, f);
    }

    pub fn get_code_lens_resolve(
        &self,
        code_lens: CodeLens,
        path: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::GetCodeLensResolve { code_lens, path }, f);
    }

    pub fn get_document_formatting(
        &self,
        path: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::GetDocumentFormatting { path }, f);
    }

    pub fn get_semantic_tokens(
        &self,
        path: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::GetSemanticTokens { path }, f);
    }

    pub fn prepare_rename(
        &self,
        path: PathBuf,
        position: Position,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::PrepareRename { path, position }, f);
    }

    pub fn rename(
        &self,
        path: PathBuf,
        position: Position,
        new_name: String,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::Rename {
                path,
                position,
                new_name,
            },
            f,
        );
    }

    pub fn get_inlay_hints(&self, path: PathBuf, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::GetInlayHints { path }, f);
    }

    pub fn get_document_diagnostics(
        &self,
        path: PathBuf,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::GetDocumentDiagnostics { path }, f);
    }

    pub fn get_inline_completions(
        &self,
        path: PathBuf,
        position: Position,
        trigger_kind: InlineCompletionTriggerKind,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(
            ProxyRequest::GetInlineCompletions {
                path,
                position,
                trigger_kind,
            },
            f,
        );
    }

    pub fn update(&self, path: PathBuf, delta: RopeDelta, rev: u64) {
        self.notification(ProxyNotification::Update { path, delta, rev });
    }

    pub fn get_selection_range(
        &self,
        path: PathBuf,
        positions: Vec<Position>,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::GetSelectionRange { path, positions }, f);
    }

    pub fn get_workspace_symbols(
        &self,
        path: PathBuf,
        query: String,
        f: impl ProxyCallback + 'static,
    ) {
        self.request_async(ProxyRequest::GetWorkspaceSymbols { path, query }, f);
    }

    pub fn list_all_folders(&self, f: impl ProxyCallback + 'static) {
        self.request_async(ProxyRequest::ListAllFolders {}, f);
    }
}

impl Default for ProxyRpcHandler {
    fn default() -> Self {
        Self::new()
    }
}
