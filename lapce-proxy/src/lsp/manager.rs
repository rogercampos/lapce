use std::{borrow::Cow, collections::HashMap, path::PathBuf};

use lapce_rpc::{
    RpcError, plugin::PluginId, proxy::ProxyResponse, style::LineStyle,
};
use lapce_xi_rope::{Rope, RopeDelta};
use lsp_types::{
    DidOpenTextDocumentParams, SemanticTokens, TextDocumentIdentifier,
    TextDocumentItem, VersionedTextDocumentIdentifier,
    notification::{DidOpenTextDocument, Notification},
};
use serde_json::Value;

use super::{ClonableCallback, LspRpcHandler, RpcCallback, client::LspClient};

/// Describes one built-in LSP server.
pub struct LspServerConfig {
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub languages: &'static [&'static str],
    /// JSON string for LSP initializationOptions, parsed at server startup.
    pub init_options_json: Option<&'static str>,
}

/// Built-in LSP server registry. Adding a new language server = adding one entry here.
pub const LSP_SERVERS: &[LspServerConfig] = &[LspServerConfig {
    command: "ruby-lsp",
    args: &[],
    languages: &["ruby"],
    init_options_json: Some(r#"{"enabledFeatures":{"semanticHighlighting":false}}"#),
}];

/// Manages multiple LSP server instances. Runs on a dedicated thread.
pub struct LspManager {
    workspace: Option<PathBuf>,
    lsp_rpc: LspRpcHandler,
    /// Active LSP server instances, keyed by PluginId
    servers: HashMap<PluginId, LspClient>,
    /// Maps language_id → PluginId for routing
    language_to_server: HashMap<String, PluginId>,
    /// Tracks which configs have been activated
    activated_configs: Vec<usize>,
    /// Tracks open files for lazy activation replay
    open_files: HashMap<PathBuf, TextDocumentItem>,
}

impl LspManager {
    pub fn new(workspace: Option<PathBuf>, lsp_rpc: LspRpcHandler) -> Self {
        Self {
            workspace,
            lsp_rpc,
            servers: HashMap::new(),
            language_to_server: HashMap::new(),
            activated_configs: Vec::new(),
            open_files: HashMap::new(),
        }
    }

    /// Try to activate a server for the given language if one isn't already running.
    fn ensure_server_for_language(&mut self, language_id: &str) {
        if self.language_to_server.contains_key(language_id) {
            return;
        }

        // Find a matching config that hasn't been activated yet
        for (idx, config) in LSP_SERVERS.iter().enumerate() {
            if self.activated_configs.contains(&idx) {
                continue;
            }
            if !config.languages.contains(&language_id) {
                continue;
            }

            // Try to start this server
            let env = self.lsp_rpc.shell_env.clone();
            match LspClient::start(
                self.lsp_rpc.clone(),
                self.workspace.clone(),
                config.command,
                config.command,
                config.args,
                config.languages,
                config
                    .init_options_json
                    .and_then(|s| serde_json::from_str(s).ok()),
                env,
            ) {
                Ok(client) => {
                    let plugin_id = client.plugin_id;
                    self.activated_configs.push(idx);

                    // Register language → server mapping for all languages this server handles
                    for lang in config.languages {
                        self.language_to_server.insert(lang.to_string(), plugin_id);
                    }

                    self.servers.insert(plugin_id, client);

                    // Replay open files for this server
                    self.replay_open_files(plugin_id);
                }
                Err(err) => {
                    tracing::error!(
                        "Failed to start LSP server {}: {:?}",
                        config.command,
                        err
                    );
                }
            }
            break;
        }
    }

    /// Replay didOpen for all currently open files that match this server.
    fn replay_open_files(&self, plugin_id: PluginId) {
        let Some(server) = self.servers.get(&plugin_id) else {
            return;
        };

        // Also get open files from the proxy
        match self.lsp_rpc.proxy_rpc.get_open_files_content() {
            Ok(ProxyResponse::GetOpenFilesContentResponse { items }) => {
                for item in items {
                    let language_id = Some(item.language_id.clone());
                    let path = item.uri.to_file_path().ok();
                    server.server_notification(
                        DidOpenTextDocument::METHOD,
                        DidOpenTextDocumentParams {
                            text_document: item,
                        },
                        language_id,
                        path,
                    );
                }
            }
            Ok(_) => {}
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }
    }

    /// Route a request to the appropriate server based on language_id.
    pub fn handle_server_request(
        &mut self,
        plugin_id: Option<PluginId>,
        method: Cow<'static, str>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
        f: Box<dyn ClonableCallback<Value, RpcError>>,
    ) {
        // If a specific plugin_id is requested, route directly
        if let Some(plugin_id) = plugin_id {
            if let Some(server) = self.servers.get(&plugin_id) {
                server.server_request_async(
                    method,
                    params,
                    language_id,
                    path,
                    move |result| {
                        f(plugin_id, result);
                    },
                );
            } else {
                f(plugin_id, Err(RpcError::new("server doesn't exist")));
            }
            return;
        }

        // Route by language
        let target_plugin_id = language_id
            .as_deref()
            .and_then(|lang| self.language_to_server.get(lang))
            .copied();

        if let Some(pid) = target_plugin_id {
            if let Some(server) = self.servers.get(&pid) {
                server.server_request_async(
                    method,
                    params,
                    language_id,
                    path,
                    move |result| {
                        f(pid, result);
                    },
                );
                return;
            }
        }

        // No server available
        f(
            PluginId(0),
            Err(RpcError::new("no LSP server available for this language")),
        );
    }

    pub fn handle_server_notification(
        &mut self,
        plugin_id: Option<PluginId>,
        method: impl Into<Cow<'static, str>>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
    ) {
        let method = method.into();

        // If a specific plugin_id is requested, route directly
        if let Some(plugin_id) = plugin_id {
            if let Some(server) = self.servers.get(&plugin_id) {
                server.server_notification(method, params, language_id, path);
            }
            return;
        }

        // Route by language
        let target_plugin_id = language_id
            .as_deref()
            .and_then(|lang| self.language_to_server.get(lang))
            .copied();

        if let Some(pid) = target_plugin_id {
            if let Some(server) = self.servers.get(&pid) {
                server.server_notification(method, params, language_id, path);
            }
        }
    }

    pub fn handle_did_open_text_document(&mut self, document: TextDocumentItem) {
        // Track the open file
        if let Ok(path) = document.uri.to_file_path() {
            self.open_files.insert(path, document.clone());
        }

        // Ensure a server is running for this language (lazy activation)
        self.ensure_server_for_language(&document.language_id);

        // Forward to matching server
        let target_plugin_id =
            self.language_to_server.get(&document.language_id).copied();

        if let Some(pid) = target_plugin_id {
            let path = document.uri.to_file_path().ok();
            if let Some(server) = self.servers.get(&pid) {
                server.server_notification(
                    DidOpenTextDocument::METHOD,
                    DidOpenTextDocumentParams {
                        text_document: document.clone(),
                    },
                    Some(document.language_id),
                    path,
                );
            }
        }
    }

    pub fn handle_did_save_text_document(
        &mut self,
        language_id: String,
        path: PathBuf,
        text_document: TextDocumentIdentifier,
        text: Rope,
    ) {
        let target_plugin_id = self.language_to_server.get(&language_id).copied();

        if let Some(pid) = target_plugin_id {
            if let Some(server) = self.servers.get(&pid) {
                server.handle_did_save_text_document(
                    language_id,
                    path,
                    text_document,
                    text,
                );
            }
        }
    }

    pub fn handle_did_change_text_document(
        &mut self,
        language_id: String,
        document: VersionedTextDocumentIdentifier,
        delta: RopeDelta,
        text: Rope,
        new_text: Rope,
    ) {
        let target_plugin_id = self.language_to_server.get(&language_id).copied();

        if let Some(pid) = target_plugin_id {
            if let Some(server) = self.servers.get_mut(&pid) {
                server.handle_did_change_text_document(
                    language_id,
                    document,
                    delta,
                    text,
                    new_text,
                );
            }
        }
    }

    pub fn format_semantic_tokens(
        &self,
        plugin_id: PluginId,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    ) {
        if let Some(server) = self.servers.get(&plugin_id) {
            server.format_semantic_tokens(tokens, text, f);
        } else {
            f.call(Err(RpcError::new("server doesn't exist")));
        }
    }

    pub fn shutdown(&mut self) {
        for (_, server) in self.servers.iter_mut() {
            server.shutdown_process();
        }
    }
}
