pub mod client;
pub mod manager;

use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use lapce_rpc::project::ProjectInfo;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use dyn_clone::DynClone;
use lapce_rpc::{
    RpcError,
    core::{BackgroundTaskId, CoreRpcHandler},
    plugin::PluginId,
    proxy::ProxyRpcHandler,
    style::LineStyle,
};
use lapce_xi_rope::{Rope, RopeDelta};
use lsp_types::{
    CallHierarchyClientCapabilities, CallHierarchyIncomingCall,
    CallHierarchyIncomingCallsParams, CallHierarchyItem, CallHierarchyPrepareParams,
    ClientCapabilities, CodeAction, CodeActionCapabilityResolveSupport,
    CodeActionClientCapabilities, CodeActionContext, CodeActionKind,
    CodeActionKindLiteralSupport, CodeActionLiteralSupport, CodeActionParams,
    CodeActionResponse, CodeLens, CodeLensParams, CompletionClientCapabilities,
    CompletionItem, CompletionItemCapability,
    CompletionItemCapabilityResolveSupport, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticClientCapabilities, DocumentDiagnosticParams,
    DocumentDiagnosticReportResult, DocumentFormattingParams,
    DocumentSymbolClientCapabilities, FoldingRange, FoldingRangeClientCapabilities,
    FoldingRangeParams, FormattingOptions, GotoCapability, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverClientCapabilities, HoverParams, InlayHint,
    InlayHintClientCapabilities, InlayHintParams,
    InlineCompletionClientCapabilities, InlineCompletionParams,
    InlineCompletionResponse, InlineCompletionTriggerKind, Location, MarkupKind,
    MessageActionItemCapabilities, ParameterInformationSettings,
    PartialResultParams, Position, PrepareRenameResponse,
    PublishDiagnosticsClientCapabilities, Range, ReferenceContext, ReferenceParams,
    RenameParams, SelectionRange, SelectionRangeParams, SemanticTokens,
    SemanticTokensClientCapabilities, SemanticTokensParams,
    ShowMessageRequestClientCapabilities, SignatureHelp,
    SignatureHelpClientCapabilities, SignatureHelpParams,
    SignatureInformationSettings, TextDocumentClientCapabilities,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    TextDocumentSyncClientCapabilities, TextEdit, Url,
    VersionedTextDocumentIdentifier, WindowClientCapabilities,
    WorkDoneProgressParams, WorkspaceClientCapabilities, WorkspaceEdit,
    WorkspaceSymbolClientCapabilities, WorkspaceSymbolParams,
    request::{
        CallHierarchyIncomingCalls, CallHierarchyPrepare, CodeActionRequest,
        CodeActionResolveRequest, CodeLensRequest, CodeLensResolve, Completion,
        DocumentDiagnosticRequest, FoldingRangeRequest, Formatting, GotoDefinition,
        GotoImplementation, GotoImplementationResponse, GotoTypeDefinition,
        GotoTypeDefinitionParams, GotoTypeDefinitionResponse, HoverRequest,
        InlayHintRequest, InlineCompletionRequest, PrepareRenameRequest, References,
        Rename, Request, ResolveCompletionItem, SelectionRangeRequest,
        SemanticTokensFullRequest, SignatureHelpRequest, WorkspaceSymbolRequest,
    },
};
use parking_lot::Mutex;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use self::manager::LspManager;
use crate::buffer::language_id_from_path;

/// Callback trait that is both FnOnce and Clone-able. The DynClone requirement
/// exists because when broadcasting a request to all plugins, the callback must
/// be cloned once per plugin.
pub trait ClonableCallback<Resp, Error>:
    FnOnce(PluginId, Result<Resp, Error>) + Send + DynClone
{
}

impl<Resp, Error, F: Send + FnOnce(PluginId, Result<Resp, Error>) + DynClone>
    ClonableCallback<Resp, Error> for F
{
}

pub trait RpcCallback<Resp, Error>: Send {
    fn call(self: Box<Self>, result: Result<Resp, Error>);
}

impl<Resp, Error, F: Send + FnOnce(Result<Resp, Error>)> RpcCallback<Resp, Error>
    for F
{
    fn call(self: Box<F>, result: Result<Resp, Error>) {
        (*self)(result)
    }
}

/// Messages sent to the LspManager's dedicated mainloop thread.
#[allow(clippy::large_enum_variant)]
pub enum LspRpc {
    ServerRequest {
        plugin_id: Option<PluginId>,
        method: Cow<'static, str>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
        f: Box<dyn ClonableCallback<Value, RpcError>>,
    },
    ServerNotification {
        plugin_id: Option<PluginId>,
        method: Cow<'static, str>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
    },
    FormatSemanticTokens {
        plugin_id: PluginId,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    },
    DidOpenTextDocument {
        document: TextDocumentItem,
    },
    DidChangeTextDocument {
        language_id: String,
        document: VersionedTextDocumentIdentifier,
        delta: RopeDelta,
        text: Rope,
        new_text: Rope,
    },
    DidSaveTextDocument {
        language_id: String,
        path: PathBuf,
        text_document: TextDocumentIdentifier,
        text: Rope,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct LspRpcHandler {
    core_rpc: CoreRpcHandler,
    proxy_rpc: ProxyRpcHandler,
    lsp_tx: Sender<LspRpc>,
    lsp_rx: Arc<Mutex<Option<Receiver<LspRpc>>>>,
    /// Default shell environment (resolved for the workspace root).
    pub default_shell_env: Arc<Mutex<Arc<HashMap<String, String>>>>,
    /// Per-project shell environments, keyed by project root path.
    /// Lazily populated: entries are resolved on first access.
    pub project_shell_envs:
        Arc<Mutex<HashMap<PathBuf, Arc<HashMap<String, String>>>>>,
}

impl LspRpcHandler {
    pub fn new(core_rpc: CoreRpcHandler, proxy_rpc: ProxyRpcHandler) -> Self {
        let (lsp_tx, lsp_rx) = crossbeam_channel::unbounded();
        Self {
            core_rpc,
            proxy_rpc,
            lsp_tx,
            lsp_rx: Arc::new(Mutex::new(Some(lsp_rx))),
            default_shell_env: Arc::new(Mutex::new(Arc::new(HashMap::new()))),
            project_shell_envs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn set_default_shell_env(&self, default_env: HashMap<String, String>) {
        *self.default_shell_env.lock() = Arc::new(default_env);
    }

    /// Show a user-visible message in the UI.
    pub fn show_message(
        &self,
        title: String,
        message: lsp_types::ShowMessageParams,
    ) {
        self.core_rpc.show_message(title, message);
    }

    /// Get the shell environment for a specific project root, resolving lazily
    /// if not yet cached. Falls back to the default workspace-root environment
    /// when no project root is given.
    pub fn shell_env_for_project(
        &self,
        project_root: Option<&Path>,
    ) -> Arc<HashMap<String, String>> {
        let Some(root) = project_root else {
            return self.default_shell_env.lock().clone();
        };

        // Check cache first
        {
            let cache = self.project_shell_envs.lock();
            if let Some(env) = cache.get(root) {
                return env.clone();
            }
        }

        // Not cached — resolve lazily
        let dir_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let task_id = self.next_background_task_id();
        let name = format!("Resolving environment: {dir_name}");
        self.background_task_started(task_id, name);

        let env = crate::shell_env::resolve_shell_env(Some(root));
        let arc_env = Arc::new(env);

        self.project_shell_envs
            .lock()
            .insert(root.to_path_buf(), arc_env.clone());
        self.background_task_finished(task_id);

        arc_env
    }

    /// Forward a projects_detected notification to the UI.
    pub fn projects_detected(&self, projects: Vec<ProjectInfo>) {
        self.core_rpc.projects_detected(projects);
    }

    pub fn next_background_task_id(&self) -> BackgroundTaskId {
        self.core_rpc.next_background_task_id()
    }

    pub fn background_task_queued(&self, task_id: BackgroundTaskId, name: String) {
        self.core_rpc.background_task_queued(task_id, name);
    }

    pub fn background_task_started(&self, task_id: BackgroundTaskId, name: String) {
        self.core_rpc.background_task_started(task_id, name);
    }

    pub fn background_task_progress(
        &self,
        task_id: BackgroundTaskId,
        message: Option<String>,
        percentage: Option<u32>,
    ) {
        self.core_rpc
            .background_task_progress(task_id, message, percentage);
    }

    pub fn background_task_finished(&self, task_id: BackgroundTaskId) {
        self.core_rpc.background_task_finished(task_id);
    }

    pub fn mainloop(&self, manager: &mut LspManager) {
        let lsp_rx = self.lsp_rx.lock().take().unwrap();
        for msg in lsp_rx {
            match msg {
                LspRpc::ServerRequest {
                    plugin_id,
                    method,
                    params,
                    language_id,
                    path,
                    f,
                } => {
                    manager.handle_server_request(
                        plugin_id,
                        method,
                        params,
                        language_id,
                        path,
                        f,
                    );
                }
                LspRpc::ServerNotification {
                    plugin_id,
                    method,
                    params,
                    language_id,
                    path,
                } => {
                    manager.handle_server_notification(
                        plugin_id,
                        method,
                        params,
                        language_id,
                        path,
                    );
                }
                LspRpc::FormatSemanticTokens {
                    plugin_id,
                    tokens,
                    text,
                    f,
                } => {
                    manager.format_semantic_tokens(plugin_id, tokens, text, f);
                }
                LspRpc::DidOpenTextDocument { document } => {
                    manager.handle_did_open_text_document(document);
                }
                LspRpc::DidSaveTextDocument {
                    language_id,
                    path,
                    text_document,
                    text,
                } => {
                    manager.handle_did_save_text_document(
                        language_id,
                        path,
                        text_document,
                        text,
                    );
                }
                LspRpc::DidChangeTextDocument {
                    language_id,
                    document,
                    delta,
                    text,
                    new_text,
                } => {
                    manager.handle_did_change_text_document(
                        language_id,
                        document,
                        delta,
                        text,
                        new_text,
                    );
                }
                LspRpc::Shutdown => {
                    manager.shutdown();
                    return;
                }
            }
        }
    }

    pub fn shutdown(&self) {
        if let Err(err) = self.lsp_tx.send(LspRpc::Shutdown) {
            tracing::error!("{:?}", err);
        }
    }

    pub(crate) fn send_request<P: Serialize>(
        &self,
        plugin_id: Option<PluginId>,
        method: impl Into<Cow<'static, str>>,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
        f: impl FnOnce(PluginId, Result<Value, RpcError>) + Send + DynClone + 'static,
    ) {
        let params = serde_json::to_value(params).unwrap();
        let rpc = LspRpc::ServerRequest {
            plugin_id,
            method: method.into(),
            params,
            language_id,
            path,
            f: Box::new(f),
        };
        if let Err(err) = self.lsp_tx.send(rpc) {
            tracing::error!("{:?}", err);
        }
    }

    pub(crate) fn send_notification<P: Serialize>(
        &self,
        plugin_id: Option<PluginId>,
        method: impl Into<Cow<'static, str>>,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
    ) {
        let params = serde_json::to_value(params).unwrap();
        let rpc = LspRpc::ServerNotification {
            plugin_id,
            method: method.into(),
            params,
            language_id,
            path,
        };
        if let Err(err) = self.lsp_tx.send(rpc) {
            tracing::error!("{:?}", err);
        }
    }

    fn send_lsp_request<P, Resp>(
        &self,
        path: &Path,
        method: &'static str,
        params: P,
        cb: impl FnOnce(PluginId, Result<Resp, RpcError>) + Clone + Send + 'static,
    ) where
        P: Serialize,
        Resp: DeserializeOwned,
    {
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        let got_success = Arc::new(AtomicBool::new(false));
        self.send_request(
            None,
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            move |plugin_id, result| {
                if got_success.load(Ordering::Acquire) {
                    return;
                }
                let result = match result {
                    Ok(value) => {
                        if let Ok(item) = serde_json::from_value::<Resp>(value) {
                            got_success.store(true, Ordering::Release);
                            Ok(item)
                        } else {
                            Err(RpcError::new("deserialize error"))
                        }
                    }
                    Err(e) => Err(e),
                };
                cb(plugin_id, result)
            },
        );
    }

    pub fn format_semantic_tokens(
        &self,
        plugin_id: PluginId,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    ) {
        if let Err(err) = self.lsp_tx.send(LspRpc::FormatSemanticTokens {
            plugin_id,
            tokens,
            text,
            f,
        }) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn did_save_text_document(&self, path: &Path, text: Rope) {
        let text_document =
            TextDocumentIdentifier::new(Url::from_file_path(path).unwrap());
        let language_id = language_id_from_path(path).unwrap_or("").to_string();
        if let Err(err) = self.lsp_tx.send(LspRpc::DidSaveTextDocument {
            language_id,
            text_document,
            path: path.into(),
            text,
        }) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn did_change_text_document(
        &self,
        path: &Path,
        rev: u64,
        delta: RopeDelta,
        text: Rope,
        new_text: Rope,
    ) {
        let document = VersionedTextDocumentIdentifier::new(
            Url::from_file_path(path).unwrap(),
            rev as i32,
        );
        let language_id = language_id_from_path(path).unwrap_or("").to_string();
        if let Err(err) = self.lsp_tx.send(LspRpc::DidChangeTextDocument {
            language_id,
            document,
            delta,
            text,
            new_text,
        }) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn did_open_document(
        &self,
        path: &Path,
        language_id: String,
        version: i32,
        text: String,
    ) {
        match Url::from_file_path(path) {
            Ok(path) => {
                if let Err(err) = self.lsp_tx.send(LspRpc::DidOpenTextDocument {
                    document: TextDocumentItem::new(
                        path,
                        language_id,
                        version,
                        text,
                    ),
                }) {
                    tracing::error!("{:?}", err);
                }
            }
            Err(_) => {
                tracing::error!("Failed to parse URL from file path: {path:?}");
            }
        }
    }

    pub fn get_definition(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<GotoDefinitionResponse, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = GotoDefinition::METHOD;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_type_definition(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<GotoTypeDefinitionResponse, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = GotoTypeDefinition::METHOD;
        let params = GotoTypeDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn call_hierarchy_incoming(
        &self,
        path: &Path,
        item: CallHierarchyItem,
        cb: impl FnOnce(
            PluginId,
            Result<Option<Vec<CallHierarchyIncomingCall>>, RpcError>,
        ) + Clone
        + Send
        + 'static,
    ) {
        let method = CallHierarchyIncomingCalls::METHOD;
        let params = CallHierarchyIncomingCallsParams {
            item,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn show_call_hierarchy(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Option<Vec<CallHierarchyItem>>, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = CallHierarchyPrepare::METHOD;
        let params = CallHierarchyPrepareParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_references(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Vec<Location>, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = References::METHOD;
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration: false,
            },
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_lsp_folding_range(
        &self,
        path: &Path,
        cb: impl FnOnce(
            PluginId,
            std::result::Result<Option<Vec<FoldingRange>>, RpcError>,
        ) + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = FoldingRangeRequest::METHOD;
        let params = FoldingRangeParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn go_to_implementation(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Option<GotoImplementationResponse>, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = GotoImplementation::METHOD;
        let params = GotoTypeDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_code_actions(
        &self,
        path: &Path,
        position: Position,
        diagnostics: Vec<Diagnostic>,
        cb: impl FnOnce(PluginId, Result<CodeActionResponse, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = CodeActionRequest::METHOD;
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range: Range {
                start: position,
                end: position,
            },
            context: CodeActionContext {
                diagnostics,
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_code_lens(
        &self,
        path: &Path,
        cb: impl FnOnce(PluginId, Result<Option<Vec<CodeLens>>, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = CodeLensRequest::METHOD;
        let params = CodeLensParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_code_lens_resolve(
        &self,
        path: &Path,
        code_lens: &CodeLens,
        cb: impl FnOnce(PluginId, Result<CodeLens, RpcError>) + Clone + Send + 'static,
    ) {
        let method = CodeLensResolve::METHOD;
        self.send_lsp_request(path, method, code_lens, cb);
    }

    pub fn get_inlay_hints(
        &self,
        path: &Path,
        range: Range,
        cb: impl FnOnce(PluginId, Result<Vec<InlayHint>, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = InlayHintRequest::METHOD;
        let params = InlayHintParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            range,
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_document_diagnostics(
        &self,
        path: &Path,
        cb: impl FnOnce(PluginId, Result<DocumentDiagnosticReportResult, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = DocumentDiagnosticRequest::METHOD;
        let params = DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri },
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_inline_completions(
        &self,
        path: &Path,
        position: Position,
        trigger_kind: InlineCompletionTriggerKind,
        cb: impl FnOnce(PluginId, Result<InlineCompletionResponse, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = InlineCompletionRequest::METHOD;
        let params = InlineCompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            context: lsp_types::InlineCompletionContext {
                trigger_kind,
                selected_completion_info: None,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_document_formatting(
        &self,
        path: &Path,
        cb: impl FnOnce(PluginId, Result<Vec<TextEdit>, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = Formatting::METHOD;
        let params = DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri },
            options: FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn prepare_rename(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<PrepareRenameResponse, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = PrepareRenameRequest::METHOD;
        let params = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position,
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn rename(
        &self,
        path: &Path,
        position: Position,
        new_name: String,
        cb: impl FnOnce(PluginId, Result<WorkspaceEdit, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = Rename::METHOD;
        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            new_name,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_semantic_tokens(
        &self,
        path: &Path,
        cb: impl FnOnce(PluginId, Result<SemanticTokens, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = SemanticTokensFullRequest::METHOD;
        let params = SemanticTokensParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn get_selection_range(
        &self,
        path: &Path,
        positions: Vec<Position>,
        cb: impl FnOnce(PluginId, Result<Vec<SelectionRange>, RpcError>)
        + Clone
        + Send
        + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = SelectionRangeRequest::METHOD;
        let params = SelectionRangeParams {
            text_document: TextDocumentIdentifier { uri },
            positions,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn workspace_symbol(
        &self,
        path: &Path,
        query: String,
        cb: impl FnOnce(
            PluginId,
            Result<Option<lsp_types::WorkspaceSymbolResponse>, RpcError>,
        ) + Clone
        + Send
        + 'static,
    ) {
        let method = WorkspaceSymbolRequest::METHOD;
        let params = WorkspaceSymbolParams {
            query,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn hover(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Hover, RpcError>) + Clone + Send + 'static,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = HoverRequest::METHOD;
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.send_lsp_request(path, method, params, cb);
    }

    pub fn completion(
        &self,
        request_id: usize,
        path: &Path,
        input: String,
        position: Position,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = Completion::METHOD;
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        };

        let core_rpc = self.core_rpc.clone();
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());

        let got_success = Arc::new(AtomicBool::new(false));
        self.send_request(
            None,
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            move |plugin_id, result| {
                if got_success.load(Ordering::Acquire) {
                    return;
                }
                match result {
                    Ok(value) => {
                        if let Ok(resp) =
                            serde_json::from_value::<CompletionResponse>(value)
                        {
                            got_success.store(true, Ordering::Release);
                            core_rpc.completion_response(
                                request_id, input, resp, plugin_id,
                            );
                        }
                    }
                    Err(err) => {
                        tracing::error!("{:?}", err);
                    }
                }
            },
        );
    }

    pub fn completion_resolve(
        &self,
        plugin_id: PluginId,
        item: CompletionItem,
        cb: impl FnOnce(Result<CompletionItem, RpcError>) + Send + Clone + 'static,
    ) {
        let method = ResolveCompletionItem::METHOD;
        self.send_request(
            Some(plugin_id),
            method,
            item,
            None,
            None,
            move |_, result| {
                let result = match result {
                    Ok(value) => {
                        if let Ok(item) =
                            serde_json::from_value::<CompletionItem>(value)
                        {
                            Ok(item)
                        } else {
                            Err(RpcError::new("completion item deserialize error"))
                        }
                    }
                    Err(e) => Err(e),
                };
                cb(result)
            },
        );
    }

    pub fn signature_help(
        &self,
        request_id: usize,
        path: &Path,
        position: Position,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = SignatureHelpRequest::METHOD;
        let params = SignatureHelpParams {
            context: None,
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };

        let core_rpc = self.core_rpc.clone();
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request(
            None,
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            move |plugin_id, result| match result {
                Ok(value) => {
                    if let Ok(resp) = serde_json::from_value::<SignatureHelp>(value)
                    {
                        core_rpc
                            .signature_help_response(request_id, resp, plugin_id);
                    }
                }
                Err(err) => {
                    tracing::error!("{:?}", err);
                }
            },
        );
    }

    pub fn action_resolve(
        &self,
        item: CodeAction,
        plugin_id: PluginId,
        cb: impl FnOnce(Result<CodeAction, RpcError>) + Send + Clone + 'static,
    ) {
        let method = CodeActionResolveRequest::METHOD;
        self.send_request(
            Some(plugin_id),
            method,
            item,
            None,
            None,
            move |_, result| {
                let result = match result {
                    Ok(value) => {
                        if let Ok(item) = serde_json::from_value::<CodeAction>(value)
                        {
                            Ok(item)
                        } else {
                            Err(RpcError::new("code_action item deserialize error"))
                        }
                    }
                    Err(e) => Err(e),
                };
                cb(result)
            },
        );
    }
}

/// Constructs the LSP ClientCapabilities that Lapce advertises to language servers.
pub(crate) fn client_capabilities() -> ClientCapabilities {
    let mut experimental = Map::new();
    experimental.insert("serverStatusNotification".into(), true.into());
    let command_vec = ["rust-analyzer.runSingle", "rust-analyzer.debugSingle"]
        .map(Value::from)
        .to_vec();

    let mut commands = Map::new();
    experimental.insert("serverStatusNotification".into(), true.into());
    commands.insert("commands".into(), command_vec.into());
    experimental.insert("commands".into(), commands.into());
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                dynamic_registration: Some(true),
                ..Default::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(true),
                    resolve_support: Some(CompletionItemCapabilityResolveSupport {
                        properties: vec!["additionalTextEdits".to_string()],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            signature_help: Some(SignatureHelpClientCapabilities {
                signature_information: Some(SignatureInformationSettings {
                    documentation_format: Some(vec![
                        MarkupKind::Markdown,
                        MarkupKind::PlainText,
                    ]),
                    parameter_information: Some(ParameterInformationSettings {
                        label_offset_support: Some(true),
                    }),
                    active_parameter_support: Some(true),
                }),
                ..Default::default()
            }),
            hover: Some(HoverClientCapabilities {
                content_format: Some(vec![
                    MarkupKind::Markdown,
                    MarkupKind::PlainText,
                ]),
                ..Default::default()
            }),
            inlay_hint: Some(InlayHintClientCapabilities {
                ..Default::default()
            }),
            code_action: Some(CodeActionClientCapabilities {
                data_support: Some(true),
                resolve_support: Some(CodeActionCapabilityResolveSupport {
                    properties: vec!["edit".to_string()],
                }),
                code_action_literal_support: Some(CodeActionLiteralSupport {
                    code_action_kind: CodeActionKindLiteralSupport {
                        value_set: vec![
                            CodeActionKind::EMPTY.as_str().to_string(),
                            CodeActionKind::QUICKFIX.as_str().to_string(),
                            CodeActionKind::REFACTOR.as_str().to_string(),
                            CodeActionKind::REFACTOR_EXTRACT.as_str().to_string(),
                            CodeActionKind::REFACTOR_INLINE.as_str().to_string(),
                            CodeActionKind::REFACTOR_REWRITE.as_str().to_string(),
                            CodeActionKind::SOURCE.as_str().to_string(),
                            CodeActionKind::SOURCE_ORGANIZE_IMPORTS
                                .as_str()
                                .to_string(),
                            "quickassist".to_string(),
                            "source.fixAll".to_string(),
                        ],
                    },
                }),
                ..Default::default()
            }),
            semantic_tokens: Some(SemanticTokensClientCapabilities {
                ..Default::default()
            }),
            type_definition: Some(GotoCapability {
                link_support: Some(false),
                ..Default::default()
            }),
            definition: Some(GotoCapability {
                ..Default::default()
            }),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                ..Default::default()
            }),
            inline_completion: Some(InlineCompletionClientCapabilities {
                ..Default::default()
            }),
            diagnostic: Some(DiagnosticClientCapabilities {
                ..Default::default()
            }),
            call_hierarchy: Some(CallHierarchyClientCapabilities {
                dynamic_registration: Some(true),
            }),
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            folding_range: Some(FoldingRangeClientCapabilities {
                dynamic_registration: Some(false),
                range_limit: None,
                line_folding_only: Some(false),
                folding_range_kind: None,
                folding_range: None,
            }),
            ..Default::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            show_message: Some(ShowMessageRequestClientCapabilities {
                message_action_item: Some(MessageActionItemCapabilities {
                    additional_properties_support: Some(true),
                }),
            }),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            symbol: Some(WorkspaceSymbolClientCapabilities {
                ..Default::default()
            }),
            configuration: Some(false),
            workspace_folders: Some(true),
            ..Default::default()
        }),
        experimental: Some(experimental.into()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_capabilities_has_text_document() {
        let caps = client_capabilities();
        assert!(caps.text_document.is_some());
    }

    #[test]
    fn client_capabilities_snippet_support() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let completion = td.completion.unwrap();
        let item = completion.completion_item.unwrap();
        assert_eq!(item.snippet_support, Some(true));
    }

    #[test]
    fn client_capabilities_completion_resolve_additional_text_edits() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let completion = td.completion.unwrap();
        let item = completion.completion_item.unwrap();
        let resolve = item.resolve_support.unwrap();
        assert_eq!(resolve.properties, vec!["additionalTextEdits"]);
    }

    #[test]
    fn client_capabilities_signature_help_markdown() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let sig = td.signature_help.unwrap();
        let info = sig.signature_information.unwrap();
        let formats = info.documentation_format.unwrap();
        assert_eq!(formats, vec![MarkupKind::Markdown, MarkupKind::PlainText]);
    }

    #[test]
    fn client_capabilities_signature_label_offset_support() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let sig = td.signature_help.unwrap();
        let info = sig.signature_information.unwrap();
        let param = info.parameter_information.unwrap();
        assert_eq!(param.label_offset_support, Some(true));
    }

    #[test]
    fn client_capabilities_signature_active_parameter_support() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let sig = td.signature_help.unwrap();
        let info = sig.signature_information.unwrap();
        assert_eq!(info.active_parameter_support, Some(true));
    }

    #[test]
    fn client_capabilities_hover_markdown() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let hover = td.hover.unwrap();
        let formats = hover.content_format.unwrap();
        assert_eq!(formats, vec![MarkupKind::Markdown, MarkupKind::PlainText]);
    }

    #[test]
    fn client_capabilities_did_save() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let sync = td.synchronization.unwrap();
        assert_eq!(sync.did_save, Some(true));
        assert_eq!(sync.dynamic_registration, Some(true));
    }

    #[test]
    fn client_capabilities_code_action_data_and_resolve() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let ca = td.code_action.unwrap();
        assert_eq!(ca.data_support, Some(true));
        let resolve = ca.resolve_support.unwrap();
        assert_eq!(resolve.properties, vec!["edit"]);
    }

    #[test]
    fn client_capabilities_code_action_kinds() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let ca = td.code_action.unwrap();
        let literal = ca.code_action_literal_support.unwrap();
        let kinds = &literal.code_action_kind.value_set;
        assert!(kinds.contains(&CodeActionKind::QUICKFIX.as_str().to_string()));
        assert!(kinds.contains(&CodeActionKind::REFACTOR.as_str().to_string()));
        assert!(kinds.contains(&CodeActionKind::SOURCE.as_str().to_string()));
        assert!(kinds.contains(&"quickassist".to_string()));
        assert!(kinds.contains(&"source.fixAll".to_string()));
    }

    #[test]
    fn client_capabilities_type_definition_no_link_support() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let typedef = td.type_definition.unwrap();
        assert_eq!(typedef.link_support, Some(false));
    }

    #[test]
    fn client_capabilities_window_work_done_progress() {
        let caps = client_capabilities();
        let window = caps.window.unwrap();
        assert_eq!(window.work_done_progress, Some(true));
    }

    #[test]
    fn client_capabilities_show_message() {
        let caps = client_capabilities();
        let window = caps.window.unwrap();
        let sm = window.show_message.unwrap();
        let item = sm.message_action_item.unwrap();
        assert_eq!(item.additional_properties_support, Some(true));
    }

    #[test]
    fn client_capabilities_workspace_folders() {
        let caps = client_capabilities();
        let ws = caps.workspace.unwrap();
        assert_eq!(ws.workspace_folders, Some(true));
        assert_eq!(ws.configuration, Some(false));
    }

    #[test]
    fn client_capabilities_experimental_has_server_status() {
        let caps = client_capabilities();
        let experimental = caps.experimental.unwrap();
        let obj = experimental.as_object().unwrap();
        assert_eq!(
            obj.get("serverStatusNotification"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn client_capabilities_experimental_has_commands() {
        let caps = client_capabilities();
        let experimental = caps.experimental.unwrap();
        let obj = experimental.as_object().unwrap();
        let commands = obj.get("commands").unwrap().as_object().unwrap();
        let cmd_list = commands.get("commands").unwrap().as_array().unwrap();
        assert!(cmd_list.contains(&Value::String("rust-analyzer.runSingle".into())));
        assert!(
            cmd_list.contains(&Value::String("rust-analyzer.debugSingle".into()))
        );
    }

    #[test]
    fn client_capabilities_call_hierarchy() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let ch = td.call_hierarchy.unwrap();
        assert_eq!(ch.dynamic_registration, Some(true));
    }

    #[test]
    fn client_capabilities_document_symbol_hierarchical() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let ds = td.document_symbol.unwrap();
        assert_eq!(ds.hierarchical_document_symbol_support, Some(true));
    }

    #[test]
    fn client_capabilities_folding_range() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let fr = td.folding_range.unwrap();
        assert_eq!(fr.dynamic_registration, Some(false));
        assert_eq!(fr.line_folding_only, Some(false));
        assert!(fr.range_limit.is_none());
    }

    #[test]
    fn client_capabilities_inlay_hint_present() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        assert!(td.inlay_hint.is_some());
    }

    #[test]
    fn client_capabilities_inline_completion_present() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        assert!(td.inline_completion.is_some());
    }

    #[test]
    fn client_capabilities_diagnostic_present() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        assert!(td.diagnostic.is_some());
    }

    #[test]
    fn client_capabilities_semantic_tokens_present() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        assert!(td.semantic_tokens.is_some());
    }

    #[test]
    fn client_capabilities_definition_present() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        assert!(td.definition.is_some());
    }

    #[test]
    fn client_capabilities_publish_diagnostics_present() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        assert!(td.publish_diagnostics.is_some());
    }
}
