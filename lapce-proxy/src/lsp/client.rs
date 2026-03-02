#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::{
    borrow::Cow,
    collections::HashMap,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    process::{self, Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Result, anyhow};
use crossbeam_channel::Sender;
use floem_editor_core::buffer::rope_text::{RopeText, RopeTextRef};
use jsonrpc_lite::{Id, JsonRpc, Params};
use lapce_core::{encoding::offset_utf16_to_utf8, meta};
use lapce_rpc::{
    RpcError,
    core::{CoreRpcHandler, LogLevel, ServerStatusParams},
    plugin::PluginId,
    style::{LineStyle, Style},
};
use lapce_xi_rope::{Rope, RopeDelta};
use lsp_types::{
    CancelParams, CodeActionProviderCapability, DidChangeTextDocumentParams,
    DidSaveTextDocumentParams, FoldingRangeProviderCapability,
    HoverProviderCapability, ImplementationProviderCapability, InitializeResult,
    LogMessageParams, MessageType, OneOf, ProgressParams, PublishDiagnosticsParams,
    SemanticTokens, SemanticTokensLegend, SemanticTokensServerCapabilities,
    ServerCapabilities, ShowMessageParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncSaveOptions, VersionedTextDocumentIdentifier,
    notification::{
        Cancel, DidChangeTextDocument, DidOpenTextDocument, DidSaveTextDocument,
        Initialized, LogMessage, Notification, Progress, PublishDiagnostics,
        ShowMessage,
    },
    request::{
        CallHierarchyIncomingCalls, CallHierarchyPrepare, CodeActionRequest,
        CodeActionResolveRequest, CodeLensRequest, CodeLensResolve, Completion,
        DocumentDiagnosticRequest, FoldingRangeRequest, Formatting, GotoDefinition,
        GotoImplementation, GotoTypeDefinition, HoverRequest, Initialize,
        InlayHintRequest, InlineCompletionRequest, PrepareRenameRequest, References,
        RegisterCapability, Rename, Request, ResolveCompletionItem,
        SelectionRangeRequest, SemanticTokensFullRequest, SignatureHelpRequest,
        WorkDoneProgressCreate, WorkspaceConfiguration, WorkspaceSymbolRequest,
    },
};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;

use super::{LspRpcHandler, RpcCallback};

const HEADER_CONTENT_LENGTH: &str = "content-length";
const HEADER_CONTENT_TYPE: &str = "content-type";

/// Dual-mode response handler: either synchronous (blocking channel) or async (callback).
pub enum ResponseHandler<Resp, Error> {
    Chan(Sender<Result<Resp, Error>>),
    Callback(Box<dyn RpcCallback<Resp, Error>>),
}

impl<Resp, Error> ResponseHandler<Resp, Error> {
    pub fn invoke(self, result: Result<Resp, Error>) {
        match self {
            ResponseHandler::Chan(tx) => {
                if let Err(err) = tx.send(result) {
                    tracing::error!("{:?}", err);
                }
            }
            ResponseHandler::Callback(f) => f.call(result),
        }
    }
}

/// Manages a single language server process. Owns the child process handle and
/// coordinates three background threads for I/O.
pub struct LspClient {
    process: Child,
    workspace: Option<PathBuf>,
    pub server_capabilities: ServerCapabilities,
    server_registrations: ServerRegistrations,

    // Per-server communication
    io_tx: Sender<JsonRpc>,
    pub plugin_id: PluginId,
    id: Arc<AtomicU64>,
    server_pending: Arc<Mutex<HashMap<Id, ResponseHandler<Value, RpcError>>>>,

    // Document selector
    languages: &'static [&'static str],
}

struct SaveRegistration {
    include_text: bool,
    filters: Vec<DocumentFilter>,
}

#[derive(Default)]
struct ServerRegistrations {
    save: Option<SaveRegistration>,
}

pub struct DocumentFilter {
    pub language_id: Option<String>,
    pub pattern: Option<globset::GlobMatcher>,
}

impl LspClient {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        lsp_rpc: LspRpcHandler,
        workspace: Option<PathBuf>,
        server_name: &str,
        command: &str,
        args: &[&str],
        languages: &'static [&'static str],
        options: Option<Value>,
        env: Arc<HashMap<String, String>>,
        semantic_tokens: bool,
        settings: Option<String>,
    ) -> Result<LspClient> {
        let mut process =
            Self::spawn_process(workspace.as_ref(), command, args, &env)?;
        let stdin = process.stdin.take().unwrap();
        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();

        let mut writer = Box::new(BufWriter::new(stdin));
        let (io_tx, io_rx) = crossbeam_channel::unbounded::<JsonRpc>();
        let plugin_id = PluginId::next();
        let id = Arc::new(AtomicU64::new(0));
        let server_pending: Arc<
            Mutex<HashMap<Id, ResponseHandler<Value, RpcError>>>,
        > = Arc::new(Mutex::new(HashMap::new()));
        let settings: Arc<Option<Value>> =
            Arc::new(settings.and_then(|s| serde_json::from_str::<Value>(&s).ok()));

        // Writer thread
        thread::spawn(move || {
            for msg in io_rx {
                if msg
                    .get_method()
                    .map(|x| x == lsp_types::request::Shutdown::METHOD)
                    .unwrap_or_default()
                {
                    break;
                }
                if let Ok(msg) = serde_json::to_string(&msg) {
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        let log_msg = redact_lsp_message_for_logging(&msg);
                        tracing::debug!("write to lsp: {}", log_msg);
                    }
                    let msg =
                        format!("Content-Length: {}\r\n\r\n{}", msg.len(), msg);
                    if let Err(err) = writer.write(msg.as_bytes()) {
                        tracing::error!("{:?}", err);
                    }
                    if let Err(err) = writer.flush() {
                        tracing::error!("{:?}", err);
                    }
                }
            }
        });

        // Reader thread
        let local_pending = server_pending.clone();
        let core_rpc = lsp_rpc.core_rpc.clone();

        let server_name_closure = server_name.to_string();
        let command_owned = command.to_string();
        let io_tx_for_reader = io_tx.clone();
        let settings_for_reader = settings.clone();
        thread::spawn(move || {
            let mut reader = Box::new(BufReader::new(stdout));
            loop {
                match read_message(&mut reader) {
                    Ok(message_str) => {
                        tracing::debug!("read from lsp: {}", message_str);
                        if let Some(resp) = handle_server_message(
                            &local_pending,
                            &core_rpc,
                            &server_name_closure,
                            &settings_for_reader,
                            &message_str,
                        ) {
                            if let Err(err) = io_tx_for_reader.send(resp) {
                                tracing::error!("{:?}", err);
                            }
                        }
                    }
                    Err(_err) => {
                        core_rpc.log(
                            LogLevel::Error,
                            format!("lsp server {command_owned} stopped!"),
                            Some(format!(
                                "lapce_proxy::lsp::client::{server_name_closure}::stopped"
                            )),
                        );
                        return;
                    }
                };
            }
        });

        // Stderr thread
        let core_rpc = lsp_rpc.core_rpc.clone();
        let server_name_closure = server_name.to_string();
        thread::spawn(move || {
            let mut reader = Box::new(BufReader::new(stderr));
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(n) => {
                        if n == 0 {
                            return;
                        }
                        core_rpc.log(
                            LogLevel::Trace,
                            line.trim_end().to_string(),
                            Some(format!(
                                "lapce_proxy::lsp::client::{server_name_closure}::stderr"
                            )),
                        );
                    }
                    Err(_) => {
                        return;
                    }
                }
            }
        });

        let mut client = LspClient {
            process,
            workspace,
            server_capabilities: ServerCapabilities::default(),
            server_registrations: ServerRegistrations::default(),
            io_tx,
            plugin_id,
            id,
            server_pending,
            languages,
        };

        // Initialize the LSP server
        client.initialize(options, semantic_tokens);

        Ok(client)
    }

    fn initialize(&mut self, options: Option<Value>, semantic_tokens: bool) {
        let root_uri = self
            .workspace
            .clone()
            .map(|p| lsp_types::Url::from_directory_path(p).unwrap());
        tracing::debug!("initialization_options {:?}", options);
        #[allow(deprecated)]
        let params = lsp_types::InitializeParams {
            process_id: Some(process::id()),
            root_uri: root_uri.clone(),
            initialization_options: options,
            capabilities: super::client_capabilities(),
            trace: Some(lsp_types::TraceValue::Verbose),
            workspace_folders: root_uri.map(|uri| {
                vec![lsp_types::WorkspaceFolder {
                    name: uri.as_str().to_string(),
                    uri,
                }]
            }),
            client_info: Some(lsp_types::ClientInfo {
                name: meta::NAME.to_owned(),
                version: Some(meta::VERSION.to_owned()),
            }),
            locale: None,
            root_path: None,
            work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
        };
        match self.server_request_sync(Initialize::METHOD, params) {
            Ok(value) => {
                let result: InitializeResult = match serde_json::from_value(value) {
                    Ok(r) => r,
                    Err(err) => {
                        tracing::error!(
                            "Failed to deserialize InitializeResult: {:?}",
                            err
                        );
                        return;
                    }
                };
                self.server_capabilities = result.capabilities;
                if !semantic_tokens {
                    self.server_capabilities.semantic_tokens_provider = None;
                }
                self.send_server_notification(
                    Initialized::METHOD,
                    Params::from(
                        serde_json::to_value(lsp_types::InitializedParams {})
                            .unwrap(),
                    ),
                );
            }
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }
    }

    fn server_request_sync<P: Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> Result<Value, RpcError> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let id = self.id.fetch_add(1, Ordering::Relaxed);
        let json_id = Id::Num(id as i64);
        let params = Params::from(serde_json::to_value(params).unwrap());
        {
            let mut pending = self.server_pending.lock();
            pending.insert(json_id.clone(), ResponseHandler::Chan(tx));
        }
        let msg = JsonRpc::request_with_params(json_id, method, params);
        self.send_server_rpc(msg);
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(result) => result,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                tracing::error!("LSP server timed out responding to {method}");
                Err(RpcError::new("timeout waiting for LSP response"))
            }
            Err(_) => Err(RpcError::new("io error")),
        }
    }

    pub fn server_request_async<P: Serialize>(
        &self,
        method: impl Into<Cow<'static, str>>,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
        f: impl RpcCallback<Value, RpcError> + 'static,
    ) {
        // Check if document is supported and method is registered
        if !self.document_supported(language_id.as_deref(), path.as_deref()) {
            Box::new(f).call(Err(RpcError::new("document not supported")));
            return;
        }
        let method = method.into();
        if !self.method_registered(&method) {
            Box::new(f).call(Err(RpcError::new("server not capable")));
            return;
        }
        let id = self.id.fetch_add(1, Ordering::Relaxed);
        let json_id = Id::Num(id as i64);
        let params = Params::from(serde_json::to_value(params).unwrap());
        {
            let mut pending = self.server_pending.lock();
            pending.insert(json_id.clone(), ResponseHandler::Callback(Box::new(f)));
        }
        let msg = JsonRpc::request_with_params(json_id, &method, params);
        self.send_server_rpc(msg);
    }

    pub fn server_notification<P: Serialize>(
        &self,
        method: impl Into<Cow<'static, str>>,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
    ) {
        if !self.document_supported(language_id.as_deref(), path.as_deref()) {
            return;
        }
        let method = method.into();
        if !self.method_registered(&method) {
            return;
        }
        let params = Params::from(serde_json::to_value(params).unwrap());
        self.send_server_notification(&method, params);
    }

    fn send_server_notification(&self, method: &str, params: Params) {
        let msg = JsonRpc::notification_with_params(method, params);
        self.send_server_rpc(msg);
    }

    fn send_server_rpc(&self, msg: JsonRpc) {
        if let Err(err) = self.io_tx.send(msg) {
            tracing::error!("{:?}", err);
        }
    }

    pub fn document_supported(
        &self,
        language_id: Option<&str>,
        _path: Option<&Path>,
    ) -> bool {
        match language_id {
            Some(lang) => self.languages.contains(&lang),
            None => true,
        }
    }

    pub fn method_registered(&self, method: &str) -> bool {
        match method {
            Initialize::METHOD => true,
            Initialized::METHOD => true,
            Completion::METHOD => {
                self.server_capabilities.completion_provider.is_some()
            }
            ResolveCompletionItem::METHOD => self
                .server_capabilities
                .completion_provider
                .as_ref()
                .and_then(|c| c.resolve_provider)
                .unwrap_or(false),
            DidOpenTextDocument::METHOD => {
                match &self.server_capabilities.text_document_sync {
                    Some(TextDocumentSyncCapability::Kind(kind)) => {
                        kind != &TextDocumentSyncKind::NONE
                    }
                    Some(TextDocumentSyncCapability::Options(options)) => options
                        .open_close
                        .or_else(|| {
                            options
                                .change
                                .map(|kind| kind != TextDocumentSyncKind::NONE)
                        })
                        .unwrap_or(false),
                    None => false,
                }
            }
            DidChangeTextDocument::METHOD => {
                match &self.server_capabilities.text_document_sync {
                    Some(TextDocumentSyncCapability::Kind(kind)) => {
                        kind != &TextDocumentSyncKind::NONE
                    }
                    Some(TextDocumentSyncCapability::Options(options)) => options
                        .change
                        .map(|kind| kind != TextDocumentSyncKind::NONE)
                        .unwrap_or(false),
                    None => false,
                }
            }
            SignatureHelpRequest::METHOD => {
                self.server_capabilities.signature_help_provider.is_some()
            }
            HoverRequest::METHOD => self
                .server_capabilities
                .hover_provider
                .as_ref()
                .map(|c| match c {
                    HoverProviderCapability::Simple(is_capable) => *is_capable,
                    HoverProviderCapability::Options(_) => true,
                })
                .unwrap_or(false),
            GotoDefinition::METHOD => self
                .server_capabilities
                .definition_provider
                .as_ref()
                .map(|d| match d {
                    OneOf::Left(is_capable) => *is_capable,
                    OneOf::Right(_) => true,
                })
                .unwrap_or(false),
            GotoTypeDefinition::METHOD => {
                self.server_capabilities.type_definition_provider.is_some()
            }
            References::METHOD => self
                .server_capabilities
                .references_provider
                .as_ref()
                .map(|r| match r {
                    OneOf::Left(is_capable) => *is_capable,
                    OneOf::Right(_) => true,
                })
                .unwrap_or(false),
            GotoImplementation::METHOD => self
                .server_capabilities
                .implementation_provider
                .as_ref()
                .map(|r| match r {
                    ImplementationProviderCapability::Simple(is_capable) => {
                        *is_capable
                    }
                    ImplementationProviderCapability::Options(_) => false,
                })
                .unwrap_or(false),
            FoldingRangeRequest::METHOD => self
                .server_capabilities
                .folding_range_provider
                .as_ref()
                .map(|r| match r {
                    FoldingRangeProviderCapability::Simple(support) => *support,
                    FoldingRangeProviderCapability::FoldingProvider(_) => true,
                    FoldingRangeProviderCapability::Options(_) => true,
                })
                .unwrap_or(false),
            CodeActionRequest::METHOD => self
                .server_capabilities
                .code_action_provider
                .as_ref()
                .map(|a| match a {
                    CodeActionProviderCapability::Simple(is_capable) => *is_capable,
                    CodeActionProviderCapability::Options(_) => true,
                })
                .unwrap_or(false),
            Formatting::METHOD => self
                .server_capabilities
                .document_formatting_provider
                .as_ref()
                .map(|f| match f {
                    OneOf::Left(is_capable) => *is_capable,
                    OneOf::Right(_) => true,
                })
                .unwrap_or(false),
            SemanticTokensFullRequest::METHOD => {
                self.server_capabilities.semantic_tokens_provider.is_some()
            }
            InlayHintRequest::METHOD => {
                self.server_capabilities.inlay_hint_provider.is_some()
            }
            DocumentDiagnosticRequest::METHOD => {
                self.server_capabilities.diagnostic_provider.is_some()
            }
            InlineCompletionRequest::METHOD => self
                .server_capabilities
                .inline_completion_provider
                .is_some(),
            PrepareRenameRequest::METHOD => {
                self.server_capabilities.rename_provider.is_some()
            }
            Rename::METHOD => self.server_capabilities.rename_provider.is_some(),
            SelectionRangeRequest::METHOD => {
                self.server_capabilities.selection_range_provider.is_some()
            }
            CodeActionResolveRequest::METHOD => {
                self.server_capabilities.code_action_provider.is_some()
            }
            CodeLensRequest::METHOD => {
                self.server_capabilities.code_lens_provider.is_some()
            }
            CodeLensResolve::METHOD => self
                .server_capabilities
                .code_lens_provider
                .as_ref()
                .and_then(|x| x.resolve_provider)
                .unwrap_or(false),
            CallHierarchyPrepare::METHOD => {
                self.server_capabilities.call_hierarchy_provider.is_some()
            }
            CallHierarchyIncomingCalls::METHOD => {
                self.server_capabilities.call_hierarchy_provider.is_some()
            }
            WorkspaceSymbolRequest::METHOD => self
                .server_capabilities
                .workspace_symbol_provider
                .as_ref()
                .map(|w| match w {
                    OneOf::Left(is_capable) => *is_capable,
                    OneOf::Right(_) => true,
                })
                .unwrap_or(false),
            _ => false,
        }
    }

    fn check_save_capability(&self, language_id: &str, path: &Path) -> (bool, bool) {
        if self.document_supported(Some(language_id), Some(path)) {
            let (should_send, include_text) =
                match self.server_capabilities.text_document_sync.as_ref() {
                    Some(TextDocumentSyncCapability::Kind(kind)) => {
                        if *kind != TextDocumentSyncKind::NONE {
                            (true, false)
                        } else {
                            (false, false)
                        }
                    }
                    Some(TextDocumentSyncCapability::Options(options)) => options
                        .save
                        .as_ref()
                        .map(|o| match o {
                            TextDocumentSyncSaveOptions::Supported(is_supported) => {
                                (*is_supported, true)
                            }
                            TextDocumentSyncSaveOptions::SaveOptions(options) => {
                                (true, options.include_text.unwrap_or(false))
                            }
                        })
                        .unwrap_or((false, false)),
                    None => (false, false),
                };
            return (should_send, include_text);
        }

        if let Some(options) = self.server_registrations.save.as_ref() {
            for filter in options.filters.iter() {
                if (filter.language_id.is_none()
                    || filter.language_id.as_deref() == Some(language_id))
                    && (filter.pattern.is_none()
                        || filter.pattern.as_ref().unwrap().is_match(path))
                {
                    return (true, options.include_text);
                }
            }
        }

        (false, false)
    }

    pub fn handle_did_save_text_document(
        &self,
        language_id: String,
        path: PathBuf,
        text_document: TextDocumentIdentifier,
        text: Rope,
    ) {
        let (should_send, include_text) =
            self.check_save_capability(language_id.as_str(), &path);
        if !should_send {
            return;
        }
        let params = DidSaveTextDocumentParams {
            text_document,
            text: if include_text {
                Some(text.to_string())
            } else {
                None
            },
        };
        self.send_server_notification(
            DidSaveTextDocument::METHOD,
            Params::from(serde_json::to_value(params).unwrap()),
        );
    }

    pub fn handle_did_change_text_document(
        &mut self,
        _language_id: String,
        document: VersionedTextDocumentIdentifier,
        delta: RopeDelta,
        text: Rope,
        new_text: Rope,
    ) {
        let kind = match &self.server_capabilities.text_document_sync {
            Some(TextDocumentSyncCapability::Kind(kind)) => *kind,
            Some(TextDocumentSyncCapability::Options(options)) => {
                options.change.unwrap_or(TextDocumentSyncKind::NONE)
            }
            None => TextDocumentSyncKind::NONE,
        };

        let change = match kind {
            TextDocumentSyncKind::FULL => TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: new_text.to_string(),
            },
            TextDocumentSyncKind::INCREMENTAL => {
                crate::buffer::get_document_content_change(&text, &delta)
                    .unwrap_or_else(|| TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: new_text.to_string(),
                    })
            }
            TextDocumentSyncKind::NONE => return,
            _ => return,
        };

        let params = DidChangeTextDocumentParams {
            text_document: document,
            content_changes: vec![change],
        };

        self.send_server_notification(
            DidChangeTextDocument::METHOD,
            Params::from(serde_json::to_value(params).unwrap()),
        );
    }

    pub fn format_semantic_tokens(
        &self,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    ) {
        let result = format_semantic_styles(
            &text,
            self.server_capabilities.semantic_tokens_provider.as_ref(),
            &tokens,
        )
        .ok_or_else(|| RpcError::new("can't get styles"));
        f.call(result);
    }

    pub fn shutdown_process(&mut self) {
        if let Err(err) = self.process.kill() {
            tracing::error!("{:?}", err);
        }
        if let Err(err) = self.process.wait() {
            tracing::error!("{:?}", err);
        }
    }

    fn spawn_process(
        workspace: Option<&PathBuf>,
        server: &str,
        args: &[&str],
        env: &HashMap<String, String>,
    ) -> Result<Child> {
        let mut process = Command::new(server);
        if let Some(workspace) = workspace {
            process.current_dir(workspace);
        }

        process.args(args);
        if !env.is_empty() {
            process.envs(env);
        }

        #[cfg(target_os = "windows")]
        let process = process.creation_flags(0x08000000);
        let child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(child)
    }
}

/// Parses a raw JSON-RPC message and routes it. Returns Some(JsonRpc) for
/// host requests that need a response sent back to the server.
fn handle_server_message(
    server_pending: &Arc<Mutex<HashMap<Id, ResponseHandler<Value, RpcError>>>>,
    core_rpc: &CoreRpcHandler,
    server_name: &str,
    server_settings: &Option<Value>,
    message: &str,
) -> Option<JsonRpc> {
    match JsonRpc::parse(message) {
        Ok(value @ JsonRpc::Request(_)) => {
            let id = value.get_id().unwrap();
            let method = value.get_method().unwrap().to_string();
            let _params = value.get_params().unwrap();

            // For simple host requests (WorkDoneProgressCreate, RegisterCapability,
            // WorkspaceConfiguration), we handle them inline since they don't need
            // mutable access to the manager.
            let result = match method.as_str() {
                WorkDoneProgressCreate::METHOD => Ok(Value::Null),
                RegisterCapability::METHOD => {
                    // Registration needs to be handled by the client on the manager thread.
                    // For now, just acknowledge it. The client will pick up registrations
                    // through another mechanism.
                    Ok(Value::Null)
                }
                WorkspaceConfiguration::METHOD => {
                    // LSP servers request configuration for specific sections.
                    // Use the pre-computed settings (built at server start time
                    // from the static config + dynamic workspace-specific values).
                    let settings = server_settings
                        .as_ref()
                        .cloned()
                        .unwrap_or(Value::Object(Default::default()));

                    let items =
                        serde_json::from_value::<lsp_types::ConfigurationParams>(
                            serde_json::to_value(_params).unwrap_or_default(),
                        )
                        .map(|p| p.items)
                        .unwrap_or_default();

                    let results: Vec<Value> = items
                        .iter()
                        .map(|item| {
                            match item.section.as_deref() {
                                // Empty or missing section: return all settings
                                Some("") | None => settings.clone(),
                                // Specific section: look it up in the settings
                                Some(section) => settings
                                    .get(section)
                                    .cloned()
                                    .unwrap_or(Value::Object(Default::default())),
                            }
                        })
                        .collect();
                    Ok(Value::Array(results))
                }
                // ESLint custom requests: the server requires specific
                // responses to proceed with linting.
                "eslint/confirmESLintExecution" => {
                    // Return 4 = approved. Without this the server never runs.
                    Ok(Value::Number(4.into()))
                }
                "eslint/openDoc" | "eslint/probeFailed" | "eslint/noLibrary"
                | "eslint/noConfig" => Ok(Value::Object(Default::default())),
                _ => Err(RpcError::new(format!("request {method} not supported"))),
            };

            let resp = match result {
                Ok(v) => JsonRpc::success(id, &v),
                Err(e) => JsonRpc::error(
                    id,
                    jsonrpc_lite::Error {
                        code: e.code,
                        message: e.message,
                        data: None,
                    },
                ),
            };
            Some(resp)
        }
        Ok(value @ JsonRpc::Notification(_)) => {
            let method = value.get_method().unwrap().to_string();
            let params = value.get_params().unwrap();

            // Handle notifications inline
            match method.as_str() {
                PublishDiagnostics::METHOD => {
                    if let Ok(diagnostics) =
                        serde_json::from_value::<PublishDiagnosticsParams>(
                            serde_json::to_value(params).unwrap_or_default(),
                        )
                    {
                        core_rpc.publish_diagnostics(diagnostics);
                    }
                }
                Progress::METHOD => {
                    match serde_json::from_value::<ProgressParams>(
                        serde_json::to_value(params).unwrap_or_default(),
                    ) {
                        Ok(progress) => {
                            core_rpc.work_done_progress(
                                progress,
                                server_name.to_string(),
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                "Failed to parse $/progress from {server_name}: {err}"
                            );
                        }
                    }
                }
                ShowMessage::METHOD => {
                    if let Ok(message) = serde_json::from_value::<ShowMessageParams>(
                        serde_json::to_value(params).unwrap_or_default(),
                    ) {
                        let title = format!("LSP: {server_name}");
                        core_rpc.show_message(title, message);
                    }
                }
                LogMessage::METHOD => {
                    if let Ok(message) = serde_json::from_value::<LogMessageParams>(
                        serde_json::to_value(params).unwrap_or_default(),
                    ) {
                        core_rpc.log_message(
                            message,
                            format!(
                                "lapce_proxy::lsp::client::{server_name}::LogMessage"
                            ),
                        );
                    }
                }
                Cancel::METHOD => {
                    if let Ok(params) = serde_json::from_value::<CancelParams>(
                        serde_json::to_value(params).unwrap_or_default(),
                    ) {
                        core_rpc.cancel(params);
                    }
                }
                lsp_types::notification::LogTrace::METHOD => {
                    if let Ok(params) =
                        serde_json::from_value::<lsp_types::LogTraceParams>(
                            serde_json::to_value(params).unwrap_or_default(),
                        )
                    {
                        tracing::debug!(
                            "[{server_name}] $/logTrace: {}{}",
                            params.message,
                            params
                                .verbose
                                .as_deref()
                                .map(|v| format!("\n{v}"))
                                .unwrap_or_default(),
                        );
                    }
                }
                "experimental/serverStatus" => {
                    if let Ok(param) = serde_json::from_value::<ServerStatusParams>(
                        serde_json::to_value(params).unwrap_or_default(),
                    ) {
                        if !param.is_ok() {
                            if let Some(msg) = &param.message {
                                core_rpc.show_message(
                                    server_name.to_string(),
                                    ShowMessageParams {
                                        typ: MessageType::ERROR,
                                        message: msg.clone(),
                                    },
                                );
                            }
                        }
                        core_rpc.server_status(param);
                    }
                }
                _ => {
                    core_rpc.log(
                        LogLevel::Warn,
                        format!("host notification {method} not handled"),
                        Some(format!(
                            "lapce_proxy::lsp::client::{server_name}::{method}"
                        )),
                    );
                }
            }
            None
        }
        Ok(value @ JsonRpc::Success(_)) => {
            let result = value.get_result().unwrap().clone();
            let id = value.get_id().unwrap();
            if let Some(handler) = server_pending.lock().remove(&id) {
                handler.invoke(Ok(result));
            }
            None
        }
        Ok(value @ JsonRpc::Error(_)) => {
            let error = value.get_error().unwrap();
            let id = value.get_id().unwrap();
            if let Some(handler) = server_pending.lock().remove(&id) {
                handler.invoke(Err(RpcError {
                    code: error.code,
                    message: error.message.clone(),
                }));
            }
            None
        }
        Err(err) => {
            eprintln!("parse error {err} message {message}");
            None
        }
    }
}

/// Redacts large fields from LSP JSON messages before logging.
/// Specifically, replaces `params.textDocument.text` with `"<redacted>"` in
/// `textDocument/didOpen` and `textDocument/didChange` notifications so that
/// full file contents don't flood the log files.
fn redact_lsp_message_for_logging(msg: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(msg) else {
        return msg.to_string();
    };

    let should_redact = value
        .get("method")
        .and_then(|m| m.as_str())
        .map(|m| {
            m == lsp_types::notification::DidOpenTextDocument::METHOD
                || m == lsp_types::notification::DidChangeTextDocument::METHOD
        })
        .unwrap_or(false);

    if should_redact {
        if let Some(text) = value.pointer_mut("/params/textDocument/text") {
            *text = serde_json::Value::String("<redacted>".into());
        }
        // didChange uses contentChanges[].text
        if let Some(changes) = value
            .pointer_mut("/params/contentChanges")
            .and_then(|v| v.as_array_mut())
        {
            for change in changes {
                if let Some(text) = change.get_mut("text") {
                    *text = serde_json::Value::String("<redacted>".into());
                }
            }
        }
    }

    serde_json::to_string(&value).unwrap_or_else(|_| msg.to_string())
}

pub enum LspHeader {
    ContentType,
    ContentLength(usize),
}

fn parse_header(s: &str) -> Result<LspHeader> {
    let split: Vec<String> =
        s.splitn(2, ": ").map(|s| s.trim().to_lowercase()).collect();
    if split.len() != 2 {
        return Err(anyhow!("Malformed"));
    };
    match split[0].as_ref() {
        HEADER_CONTENT_TYPE => Ok(LspHeader::ContentType),
        HEADER_CONTENT_LENGTH => {
            Ok(LspHeader::ContentLength(split[1].parse::<usize>()?))
        }
        _ => Err(anyhow!("Unknown parse error occurred")),
    }
}

pub fn read_message<T: BufRead>(reader: &mut T) -> Result<String> {
    let mut buffer = String::new();
    let mut content_length: Option<usize> = None;

    loop {
        buffer.clear();
        let _ = reader.read_line(&mut buffer)?;
        match &buffer {
            s if s.trim().is_empty() => break,
            s => {
                match parse_header(s)? {
                    LspHeader::ContentLength(len) => content_length = Some(len),
                    LspHeader::ContentType => (),
                };
            }
        };
    }

    let content_length = content_length
        .ok_or_else(|| anyhow!("missing content-length header: {}", buffer))?;

    let mut body_buffer = vec![0; content_length];
    reader.read_exact(&mut body_buffer)?;

    let body = String::from_utf8(body_buffer)?;
    Ok(body)
}

fn format_semantic_styles(
    text: &Rope,
    semantic_tokens_provider: Option<&SemanticTokensServerCapabilities>,
    tokens: &SemanticTokens,
) -> Option<Vec<LineStyle>> {
    let semantic_tokens_provider = semantic_tokens_provider?;
    let semantic_legends = semantic_tokens_legend(semantic_tokens_provider);

    let text = RopeTextRef::new(text);
    let mut highlights = Vec::new();
    let mut line = 0;
    let mut start = 0;
    let mut last_start = 0;
    for semantic_token in &tokens.data {
        if semantic_token.delta_line > 0 {
            line += semantic_token.delta_line as usize;
            start = text.offset_of_line(line);
        }

        let sub_text = text.char_indices_iter(start..);
        start += offset_utf16_to_utf8(sub_text, semantic_token.delta_start as usize);

        let sub_text = text.char_indices_iter(start..);
        let end =
            start + offset_utf16_to_utf8(sub_text, semantic_token.length as usize);

        let Some(kind) = semantic_legends
            .token_types
            .get(semantic_token.token_type as usize)
        else {
            continue;
        };
        let kind = kind.as_str().to_string();
        if start < last_start {
            continue;
        }
        last_start = start;
        highlights.push(LineStyle {
            start,
            end,
            style: Style {
                fg_color: Some(kind),
            },
        });
    }

    Some(highlights)
}

fn semantic_tokens_legend(
    semantic_tokens_provider: &SemanticTokensServerCapabilities,
) -> &SemanticTokensLegend {
    match semantic_tokens_provider {
        SemanticTokensServerCapabilities::SemanticTokensOptions(options) => {
            &options.legend
        }
        SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(
            options,
        ) => &options.semantic_tokens_options.legend,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parse_header_content_length() {
        match parse_header("Content-Length: 42").unwrap() {
            LspHeader::ContentLength(len) => assert_eq!(len, 42),
            LspHeader::ContentType => panic!("expected ContentLength"),
        }
    }

    #[test]
    fn parse_header_content_length_case_insensitive() {
        match parse_header("CONTENT-LENGTH: 100").unwrap() {
            LspHeader::ContentLength(len) => assert_eq!(len, 100),
            LspHeader::ContentType => panic!("expected ContentLength"),
        }
    }

    #[test]
    fn parse_header_content_type() {
        match parse_header("Content-Type: application/json").unwrap() {
            LspHeader::ContentType => {}
            LspHeader::ContentLength(_) => panic!("expected ContentType"),
        }
    }

    #[test]
    fn parse_header_malformed_no_colon() {
        assert!(parse_header("malformed").is_err());
    }

    #[test]
    fn read_message_simple() {
        let body = r#"{"jsonrpc":"2.0"}"#;
        let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = Cursor::new(msg.into_bytes());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_with_content_type_header() {
        let body = r#"{"id":1}"#;
        let msg = format!(
            "Content-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = Cursor::new(msg.into_bytes());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_missing_content_length() {
        let msg = "Content-Type: application/json\r\n\r\n{}";
        let mut reader = Cursor::new(msg.as_bytes().to_vec());
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn read_message_empty_input() {
        let mut reader = Cursor::new(Vec::new());
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn read_message_unicode_body() {
        let body = r#"{"result":"こんにちは"}"#;
        let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = Cursor::new(msg.into_bytes());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_content_length_zero() {
        let msg = "Content-Length: 0\r\n\r\n";
        let mut reader = Cursor::new(msg.as_bytes().to_vec());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, "");
    }

    // Semantic token tests
    use lsp_types::{
        SemanticToken, SemanticTokensFullOptions, SemanticTokensOptions,
    };

    fn make_legend(types: Vec<&'static str>) -> SemanticTokensLegend {
        SemanticTokensLegend {
            token_types: types
                .into_iter()
                .map(lsp_types::SemanticTokenType::new)
                .collect(),
            token_modifiers: vec![],
        }
    }

    fn make_options_provider(
        types: Vec<&'static str>,
    ) -> SemanticTokensServerCapabilities {
        SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: make_legend(types),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..Default::default()
            },
        )
    }

    fn tok(
        delta_line: u32,
        delta_start: u32,
        length: u32,
        token_type: u32,
    ) -> SemanticToken {
        SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        }
    }

    #[test]
    fn format_returns_none_when_provider_is_none() {
        let text = Rope::from("hello");
        let tokens = SemanticTokens {
            result_id: None,
            data: vec![],
        };
        let result = format_semantic_styles(&text, None, &tokens);
        assert!(result.is_none());
    }

    #[test]
    fn format_empty_tokens_returns_empty_vec() {
        let text = Rope::from("hello world");
        let provider = make_options_provider(vec!["keyword"]);
        let tokens = SemanticTokens {
            result_id: None,
            data: vec![],
        };
        let result =
            format_semantic_styles(&text, Some(&provider), &tokens).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn format_single_token_on_first_line() {
        let text = Rope::from("hello world");
        let provider = make_options_provider(vec!["keyword"]);
        let tokens = SemanticTokens {
            result_id: None,
            data: vec![tok(0, 0, 5, 0)],
        };
        let result =
            format_semantic_styles(&text, Some(&provider), &tokens).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start, 0);
        assert_eq!(result[0].end, 5);
        assert_eq!(result[0].style.fg_color.as_deref(), Some("keyword"));
    }

    #[test]
    fn format_tokens_across_lines() {
        let text = Rope::from("fn main() {\n    return 42;\n}");
        let provider = make_options_provider(vec!["keyword", "function", "number"]);
        let tokens = SemanticTokens {
            result_id: None,
            data: vec![
                tok(0, 0, 2, 0),
                tok(0, 3, 4, 1),
                tok(1, 4, 6, 0),
                tok(0, 7, 2, 2),
            ],
        };
        let result =
            format_semantic_styles(&text, Some(&provider), &tokens).unwrap();
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].start, 0);
        assert_eq!(result[0].end, 2);
        assert_eq!(result[0].style.fg_color.as_deref(), Some("keyword"));
        assert_eq!(result[2].start, 16);
        assert_eq!(result[2].end, 22);
        assert_eq!(result[2].style.fg_color.as_deref(), Some("keyword"));
    }
}
