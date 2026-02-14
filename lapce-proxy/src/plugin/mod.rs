pub mod catalog;
pub mod lsp;
pub mod psp;
pub mod wasi;

use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender};
use dyn_clone::DynClone;
use flate2::read::GzDecoder;
use lapce_core::directory::Directory;
use lapce_rpc::{
    RpcError,
    core::CoreRpcHandler,
    plugin::{PluginId, VoltInfo, VoltMetadata},
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
    WorkspaceSymbolClientCapabilities,
    request::{
        CallHierarchyIncomingCalls, CallHierarchyPrepare, CodeActionRequest,
        CodeActionResolveRequest, CodeLensRequest, CodeLensResolve, Completion,
        DocumentDiagnosticRequest, FoldingRangeRequest, Formatting, GotoDefinition,
        GotoImplementation, GotoImplementationResponse, GotoTypeDefinition,
        GotoTypeDefinitionParams, GotoTypeDefinitionResponse, HoverRequest,
        InlayHintRequest, InlineCompletionRequest, PrepareRenameRequest, References,
        Rename, Request, ResolveCompletionItem, SelectionRangeRequest,
        SemanticTokensFullRequest, SignatureHelpRequest,
    },
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use tar::Archive;
use tracing::error;

use self::{
    catalog::PluginCatalog,
    psp::{ClonableCallback, PluginServerRpcHandler, RpcCallback},
    wasi::{load_volt, start_volt},
};
use crate::buffer::language_id_from_path;

pub type PluginName = String;

/// Messages sent from any thread to the PluginCatalog's dedicated mainloop thread.
/// This is the single-writer bottleneck that serializes all plugin state mutations.
/// Large variants (like ServerRequest with boxed callbacks) are acceptable because
/// these are sent through an unbounded channel, not stored in arrays.
#[allow(clippy::large_enum_variant)]
pub enum PluginCatalogRpc {
    ServerRequest {
        plugin_id: Option<PluginId>,
        request_sent: Option<Arc<AtomicUsize>>,
        method: Cow<'static, str>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
        f: Box<dyn ClonableCallback<Value, RpcError>>,
    },
    ServerNotification {
        plugin_id: Option<PluginId>,
        method: Cow<'static, str>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
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
    Handler(PluginCatalogNotification),
    RemoveVolt {
        volt: VoltInfo,
        f: Box<dyn ClonableCallback<Value, RpcError>>,
    },
    Shutdown,
}

#[allow(clippy::large_enum_variant)]
pub enum PluginCatalogNotification {
    UpdatePluginConfigs(HashMap<String, HashMap<String, serde_json::Value>>),
    UnactivatedVolts(Vec<VoltMetadata>),
    PluginServerLoaded(PluginServerRpcHandler),
    InstallVolt(VoltInfo),
    StopVolt(VoltInfo),
    EnableVolt(VoltInfo),
    ReloadVolt(VoltMetadata),
    Shutdown,
}

#[derive(Clone)]
pub struct PluginCatalogRpcHandler {
    core_rpc: CoreRpcHandler,
    proxy_rpc: ProxyRpcHandler,
    plugin_tx: Sender<PluginCatalogRpc>,
    plugin_rx: Arc<Mutex<Option<Receiver<PluginCatalogRpc>>>>,
    pub shell_env: Arc<HashMap<String, String>>,
}

impl PluginCatalogRpcHandler {
    pub fn new(core_rpc: CoreRpcHandler, proxy_rpc: ProxyRpcHandler) -> Self {
        let (plugin_tx, plugin_rx) = crossbeam_channel::unbounded();
        Self {
            core_rpc,
            proxy_rpc,
            plugin_tx,
            plugin_rx: Arc::new(Mutex::new(Some(plugin_rx))),
            shell_env: Arc::new(HashMap::new()),
        }
    }

    pub fn set_shell_env(&mut self, env: HashMap<String, String>) {
        self.shell_env = Arc::new(env);
    }

    pub fn mainloop(&self, plugin: &mut PluginCatalog) {
        let plugin_rx = self.plugin_rx.lock().take().unwrap();
        for msg in plugin_rx {
            match msg {
                PluginCatalogRpc::ServerRequest {
                    plugin_id,
                    request_sent,
                    method,
                    params,
                    language_id,
                    path,
                    check,
                    f,
                } => {
                    plugin.handle_server_request(
                        plugin_id,
                        request_sent,
                        method,
                        params,
                        language_id,
                        path,
                        check,
                        f,
                    );
                }
                PluginCatalogRpc::ServerNotification {
                    plugin_id,
                    method,
                    params,
                    language_id,
                    path,
                    check,
                } => {
                    plugin.handle_server_notification(
                        plugin_id,
                        method,
                        params,
                        language_id,
                        path,
                        check,
                    );
                }
                PluginCatalogRpc::Handler(notification) => {
                    plugin.handle_notification(notification);
                }
                PluginCatalogRpc::FormatSemanticTokens {
                    plugin_id,
                    tokens,
                    text,
                    f,
                } => {
                    plugin.format_semantic_tokens(plugin_id, tokens, text, f);
                }
                PluginCatalogRpc::DidOpenTextDocument { document } => {
                    plugin.handle_did_open_text_document(document);
                }
                PluginCatalogRpc::DidSaveTextDocument {
                    language_id,
                    path,
                    text_document,
                    text,
                } => {
                    plugin.handle_did_save_text_document(
                        language_id,
                        path,
                        text_document,
                        text,
                    );
                }
                PluginCatalogRpc::DidChangeTextDocument {
                    language_id,
                    document,
                    delta,
                    text,
                    new_text,
                } => {
                    plugin.handle_did_change_text_document(
                        language_id,
                        document,
                        delta,
                        text,
                        new_text,
                    );
                }
                PluginCatalogRpc::Shutdown => {
                    return;
                }
                PluginCatalogRpc::RemoveVolt { volt, f } => {
                    plugin.shutdown_volt(volt, f);
                }
            }
        }
    }

    pub fn shutdown(&self) {
        if let Err(err) =
            self.catalog_notification(PluginCatalogNotification::Shutdown)
        {
            tracing::error!("{:?}", err);
        }
        if let Err(err) = self.plugin_tx.send(PluginCatalogRpc::Shutdown) {
            tracing::error!("{:?}", err);
        }
    }

    fn catalog_notification(
        &self,
        notification: PluginCatalogNotification,
    ) -> Result<()> {
        self.plugin_tx
            .send(PluginCatalogRpc::Handler(notification))
            .map_err(|e| anyhow!(e.to_string()))?;
        Ok(())
    }

    /// Broadcasts a request to ALL registered plugins and uses "first success wins"
    /// semantics. This is crucial because multiple language servers may be running
    /// (e.g., rust-analyzer and a general-purpose LSP), but only one will actually
    /// handle a given file. We fire the request at all of them and take the first
    /// successful response.
    ///
    /// The callback is only invoked once: either on first success, or on final error
    /// (when all plugins have responded with errors). The `got_success` atomic
    /// prevents duplicate callback invocations across threads.
    fn send_request_to_all_plugins<P, Resp>(
        &self,
        method: &'static str,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
        cb: impl FnOnce(PluginId, Result<Resp, RpcError>) + Clone + Send + 'static,
    ) where
        P: Serialize,
        Resp: DeserializeOwned,
    {
        let got_success = Arc::new(AtomicBool::new(false));
        let request_sent = Arc::new(AtomicUsize::new(0));
        let err_received = Arc::new(AtomicUsize::new(0));
        self.send_request(
            None,
            Some(request_sent.clone()),
            method,
            params,
            language_id,
            path,
            true,
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
                if result.is_ok() {
                    cb(plugin_id, result)
                } else {
                    let rx = err_received.fetch_add(1, Ordering::Relaxed) + 1;
                    if request_sent.load(Ordering::Acquire) == rx {
                        cb(plugin_id, result)
                    }
                }
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn send_request<P: Serialize>(
        &self,
        plugin_id: Option<PluginId>,
        request_sent: Option<Arc<AtomicUsize>>,
        method: impl Into<Cow<'static, str>>,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
        f: impl FnOnce(PluginId, Result<Value, RpcError>) + Send + DynClone + 'static,
    ) {
        let params = serde_json::to_value(params).unwrap();
        let rpc = PluginCatalogRpc::ServerRequest {
            plugin_id,
            request_sent,
            method: method.into(),
            params,
            language_id,
            path,
            check,
            f: Box::new(f),
        };
        if let Err(err) = self.plugin_tx.send(rpc) {
            tracing::error!("{:?}", err);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn send_notification<P: Serialize>(
        &self,
        plugin_id: Option<PluginId>,
        method: impl Into<Cow<'static, str>>,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
    ) {
        let params = serde_json::to_value(params).unwrap();
        let rpc = PluginCatalogRpc::ServerNotification {
            plugin_id,
            method: method.into(),
            params,
            language_id,
            path,
            check,
        };
        if let Err(err) = self.plugin_tx.send(rpc) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn format_semantic_tokens(
        &self,
        plugin_id: PluginId,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    ) {
        if let Err(err) =
            self.plugin_tx.send(PluginCatalogRpc::FormatSemanticTokens {
                plugin_id,
                tokens,
                text,
                f,
            })
        {
            tracing::error!("{:?}", err);
        }
    }

    pub fn did_save_text_document(&self, path: &Path, text: Rope) {
        let text_document =
            TextDocumentIdentifier::new(Url::from_file_path(path).unwrap());
        let language_id = language_id_from_path(path).unwrap_or("").to_string();
        if let Err(err) =
            self.plugin_tx.send(PluginCatalogRpc::DidSaveTextDocument {
                language_id,
                text_document,
                path: path.into(),
                text,
            })
        {
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
        if let Err(err) =
            self.plugin_tx
                .send(PluginCatalogRpc::DidChangeTextDocument {
                    language_id,
                    document,
                    delta,
                    text,
                    new_text,
                })
        {
            tracing::error!("{:?}", err);
        }
    }

    /// Helper for LSP request methods: extracts language_id from path and
    /// dispatches to all matching plugins.
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
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            cb,
        );
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

        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            move |plugin_id, result| match result {
                Ok(value) => {
                    if let Ok(resp) =
                        serde_json::from_value::<CompletionResponse>(value)
                    {
                        core_rpc
                            .completion_response(request_id, input, resp, plugin_id);
                    }
                }
                Err(err) => {
                    tracing::error!("{:?}", err);
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
            None,
            method,
            item,
            None,
            None,
            true,
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
            // TODO: We could provide more information about the signature for the LSP to work with
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
            None,
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            true,
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
            None,
            method,
            item,
            None,
            None,
            true,
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

    pub fn did_open_document(
        &self,
        path: &Path,
        language_id: String,
        version: i32,
        text: String,
    ) {
        match Url::from_file_path(path) {
            Ok(path) => {
                if let Err(err) =
                    self.plugin_tx.send(PluginCatalogRpc::DidOpenTextDocument {
                        document: TextDocumentItem::new(
                            path,
                            language_id,
                            version,
                            text,
                        ),
                    })
                {
                    tracing::error!("{:?}", err);
                }
            }
            Err(_) => {
                tracing::error!("Failed to parse URL from file path: {path:?}");
            }
        }
    }

    pub fn unactivated_volts(&self, volts: Vec<VoltMetadata>) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::UnactivatedVolts(volts))
    }

    pub fn plugin_server_loaded(
        &self,
        plugin: PluginServerRpcHandler,
    ) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::PluginServerLoaded(
            plugin,
        ))
    }

    pub fn update_plugin_configs(
        &self,
        configs: HashMap<String, HashMap<String, serde_json::Value>>,
    ) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::UpdatePluginConfigs(
            configs,
        ))
    }

    pub fn install_volt(&self, volt: VoltInfo) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::InstallVolt(volt))
    }

    pub fn stop_volt(&self, volt: VoltInfo) {
        let rpc = PluginCatalogRpc::RemoveVolt {
            volt,
            f: Box::new(|_id: PluginId, rs: Result<Value, RpcError>| {
                if let Err(e) = rs {
                    // maybe should send notification
                    error!("{:?}", e);
                }
            }),
        };
        if let Err(err) = self.plugin_tx.send(rpc) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn remove_volt(&self, volt: VoltMetadata) {
        let catalog_rpc = self.clone();
        let volt_clone = volt.clone();
        let rpc = PluginCatalogRpc::RemoveVolt {
            volt: volt.info(),
            f: Box::new(|_id: PluginId, rs: Result<Value, RpcError>| {
                if let Err(e) = rs {
                    // maybe should send notification
                    error!("{:?}", e);
                } else if let Err(e) = remove_volt(catalog_rpc, volt_clone) {
                    error!("{:?}", e);
                }
            }),
        };
        if let Err(err) = self.plugin_tx.send(rpc) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn reload_volt(&self, volt: VoltMetadata) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::ReloadVolt(volt))
    }

    pub fn enable_volt(&self, volt: VoltInfo) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::EnableVolt(volt))
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "method", content = "params")]
pub enum PluginNotification {
    StartLspServer {
        exec_path: String,
        language_id: String,
        options: Option<Value>,
        system_lsp: Option<bool>,
    },
    DownloadFile {
        url: String,
        path: PathBuf,
    },
    LockFile {
        path: PathBuf,
    },
    MakeFileExecutable {
        path: PathBuf,
    },
}

pub fn volt_icon(volt: &VoltMetadata) -> Option<Vec<u8>> {
    let dir = volt.dir.as_ref()?;
    let icon = dir.join(volt.icon.as_ref()?);
    std::fs::read(icon).ok()
}

/// Downloads a plugin (volt) from the Lapce plugin registry. The registry API
/// returns an S3 redirect URL rather than the actual tarball, so we do two HTTP
/// fetches: one for the redirect URL, one for the actual archive.
/// Supports both zstd (preferred, smaller) and gzip compressed tarballs,
/// determined by the Content-Type header.
pub fn download_volt(volt: &VoltInfo) -> Result<VoltMetadata> {
    let url = format!(
        "https://plugins.lapce.dev/api/v1/plugins/{}/{}/{}/download",
        volt.author, volt.name, volt.version
    );

    let resp = crate::get_url(url, None)?;
    if !resp.status().is_success() {
        return Err(anyhow!("can't download plugin"));
    }

    let url = resp.text()?;

    let mut resp = crate::get_url(url, None)?;
    if !resp.status().is_success() {
        return Err(anyhow!("can't download plugin"));
    }

    let is_zstd = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        == Some("application/zstd");

    let id = volt.id();
    let plugin_dir = Directory::plugins_directory()
        .ok_or_else(|| anyhow!("can't get plugin directory"))?
        .join(id.to_string());
    if let Err(err) = fs::remove_dir_all(&plugin_dir) {
        tracing::error!("{:?}", err);
    }
    fs::create_dir_all(&plugin_dir)?;

    if is_zstd {
        let tar = zstd::Decoder::new(&mut resp).unwrap();
        let mut archive = Archive::new(tar);
        archive.unpack(&plugin_dir)?;
    } else {
        let tar = GzDecoder::new(&mut resp);
        let mut archive = Archive::new(tar);
        archive.unpack(&plugin_dir)?;
    }

    let meta = load_volt(&plugin_dir)?;
    Ok(meta)
}

pub fn install_volt(
    catalog_rpc: PluginCatalogRpcHandler,
    workspace: Option<PathBuf>,
    configurations: Option<HashMap<String, serde_json::Value>>,
    volt: VoltInfo,
) -> Result<()> {
    let download_volt_result = download_volt(&volt);
    if download_volt_result.is_err() {
        catalog_rpc
            .core_rpc
            .volt_installing(volt, "Could not download Plugin".to_string());
    }
    let meta = download_volt_result?;
    let local_catalog_rpc = catalog_rpc.clone();
    let local_meta = meta.clone();

    if let Err(err) =
        start_volt(workspace, configurations, local_catalog_rpc, local_meta)
    {
        tracing::error!("{:?}", err);
    }
    let icon = volt_icon(&meta);
    catalog_rpc.core_rpc.volt_installed(meta, icon);
    Ok(())
}

pub fn remove_volt(
    catalog_rpc: PluginCatalogRpcHandler,
    volt: VoltMetadata,
) -> Result<()> {
    std::thread::spawn(move || -> Result<()> {
        let path = volt.dir.as_ref().ok_or_else(|| {
            catalog_rpc
                .core_rpc
                .volt_removing(volt.clone(), "Plugin Directory not set".to_string());
            anyhow::anyhow!("don't have dir")
        })?;
        // Retry removal a few times — on some OSes (Windows) file locks
        // aren't released immediately after plugin deactivation.
        let mut last_err = None;
        for _ in 0..5 {
            match std::fs::remove_dir_all(path) {
                Ok(()) => {
                    catalog_rpc.core_rpc.volt_removed(volt.info(), false);
                    return Ok(());
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        let e = last_err.unwrap();
        error!("remove_dir_all {:?}", e);
        catalog_rpc.core_rpc.volt_removing(
            volt.clone(),
            format!("Could not remove plugin directory: {e}"),
        );
        Ok(())
    });
    Ok(())
}

/// Constructs the LSP ClientCapabilities that Lapce advertises to language servers.
/// This determines what features the server can use -- for example, snippet support
/// in completions, markdown in hover docs, incremental text sync, etc.
/// Must be kept in sync with what the editor actually handles.
fn client_capabilities() -> ClientCapabilities {
    // https://github.com/rust-lang/rust-analyzer/blob/master/docs/dev/lsp-extensions.md#server-status
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
                // Note: This is explicitly specified rather than left to the Default because
                // of a bug in lsp-types https://github.com/gluon-lang/lsp-types/pull/244
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
