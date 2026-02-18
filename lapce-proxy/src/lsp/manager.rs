use std::{borrow::Cow, collections::HashMap, path::PathBuf, process::Command};

use lapce_core::directory::Directory;
use lapce_rpc::{
    RpcError, plugin::PluginId, proxy::ProxyResponse, style::LineStyle,
};
use lapce_xi_rope::{Rope, RopeDelta};
use lsp_types::{
    DidOpenTextDocumentParams, MessageType, SemanticTokens, ShowMessageParams,
    TextDocumentIdentifier, TextDocumentItem, VersionedTextDocumentIdentifier,
    notification::{DidOpenTextDocument, Notification},
};
use serde_json::Value;

use super::{ClonableCallback, LspRpcHandler, RpcCallback, client::LspClient};

/// Describes one built-in LSP server.
pub struct LspServerConfig {
    /// Human-readable name shown in error messages.
    pub display_name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub languages: &'static [&'static str],
    /// JSON string for LSP initializationOptions, parsed at server startup.
    pub init_options_json: Option<&'static str>,
    /// If set, the server is an npm package that will be auto-installed
    /// into Lapce's managed `lsp-servers/` directory on first use.
    /// The `command` field should then be just the binary name (e.g.
    /// "bash-language-server"), which will be resolved to
    /// `<lsp-servers-dir>/<npm_package>/node_modules/.bin/<command>`.
    pub npm_package: Option<&'static str>,
}

/// Built-in LSP server registry. Adding a new language server = adding one entry here.
pub const LSP_SERVERS: &[LspServerConfig] = &[
    LspServerConfig {
        display_name: "ruby-lsp",
        command: "ruby-lsp",
        args: &[],
        languages: &["ruby"],
        init_options_json: Some(
            r#"{"enabledFeatures":{"semanticHighlighting":false}}"#,
        ),
        npm_package: None,
    },
    LspServerConfig {
        display_name: "bash-language-server",
        command: "bash-language-server",
        args: &["start"],
        languages: &["shellscript"],
        init_options_json: None,
        npm_package: Some("bash-language-server"),
    },
];

/// Find the LSP server command configured for a given language, if any.
pub fn lsp_command_for_language(language_id: &str) -> Option<&'static str> {
    LSP_SERVERS
        .iter()
        .find(|c| c.languages.contains(&language_id))
        .map(|c| c.command)
}

/// Manages multiple LSP server instances. Runs on a dedicated thread.
///
/// Routes LSP requests by **(language_id, project_root)** to support monorepos
/// where multiple sub-projects use the same language but need separate LSP servers
/// (e.g., two Ruby apps with different Gemfiles).
pub struct LspManager {
    workspace: Option<PathBuf>,
    lsp_rpc: LspRpcHandler,
    /// Active LSP server instances, keyed by PluginId
    servers: HashMap<PluginId, LspClient>,
    /// Maps (language_id, project_root) → PluginId for routing
    language_project_to_server: HashMap<(String, PathBuf), PluginId>,
    /// Tracks which (config_index, project_root) pairs have been activated
    activated_configs: Vec<(usize, PathBuf)>,
    /// Detected projects: (project_root, languages)
    projects: Vec<(PathBuf, Vec<String>)>,
    /// Tracks open files for lazy activation replay
    open_files: HashMap<PathBuf, TextDocumentItem>,
}

impl LspManager {
    pub fn new(
        workspace: Option<PathBuf>,
        lsp_rpc: LspRpcHandler,
        projects: Vec<(PathBuf, Vec<String>)>,
    ) -> Self {
        Self {
            workspace,
            lsp_rpc,
            servers: HashMap::new(),
            language_project_to_server: HashMap::new(),
            activated_configs: Vec::new(),
            projects,
            open_files: HashMap::new(),
        }
    }

    /// Find the longest matching project root that is a prefix of `path`.
    fn find_project_root_for_path(&self, path: &std::path::Path) -> Option<PathBuf> {
        self.projects
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map(|(root, _)| root.clone())
    }

    /// Get the effective project root for a file path, falling back to workspace.
    fn effective_project_root(&self, path: Option<&std::path::Path>) -> PathBuf {
        path.and_then(|p| self.find_project_root_for_path(p))
            .or_else(|| self.workspace.clone())
            .unwrap_or_default()
    }

    /// Try to activate a server for the given language and project root
    /// if one isn't already running.
    fn ensure_server_for_language(
        &mut self,
        language_id: &str,
        project_root: &PathBuf,
    ) {
        let key = (language_id.to_string(), project_root.clone());
        if self.language_project_to_server.contains_key(&key) {
            return;
        }

        // Find a matching config that hasn't been activated for this project root
        for (idx, config) in LSP_SERVERS.iter().enumerate() {
            let activation_key = (idx, project_root.clone());
            if self.activated_configs.contains(&activation_key) {
                continue;
            }
            if !config.languages.contains(&language_id) {
                continue;
            }

            // Resolve the actual command to execute. For npm-based servers,
            // auto-install to the managed directory if needed.
            let resolved_command = match config.npm_package {
                Some(package) => match self.resolve_npm_server(config, package) {
                    Ok(path) => path,
                    Err(err) => {
                        tracing::error!(
                            "Failed to set up npm LSP server {}: {:?}",
                            config.display_name,
                            err,
                        );
                        self.lsp_rpc.show_message(
                            format!("LSP: {}", config.display_name),
                            ShowMessageParams {
                                typ: MessageType::WARNING,
                                message: format!("{err}"),
                            },
                        );
                        break;
                    }
                },
                None => config.command.to_string(),
            };

            // Use project-specific shell env
            let env = self
                .lsp_rpc
                .shell_env_for_project(Some(project_root.as_path()));

            // Start the server with the project root as its workspace
            let server_workspace = if project_root.as_os_str().is_empty() {
                self.workspace.clone()
            } else {
                Some(project_root.clone())
            };

            match LspClient::start(
                self.lsp_rpc.clone(),
                server_workspace,
                config.display_name,
                &resolved_command,
                config.args,
                config.languages,
                config
                    .init_options_json
                    .and_then(|s| serde_json::from_str(s).ok()),
                env,
            ) {
                Ok(client) => {
                    let plugin_id = client.plugin_id;
                    self.activated_configs.push(activation_key);

                    // Register (language, project_root) → server mapping
                    for lang in config.languages {
                        self.language_project_to_server.insert(
                            (lang.to_string(), project_root.clone()),
                            plugin_id,
                        );
                    }

                    self.servers.insert(plugin_id, client);

                    // Replay open files that belong to this project root
                    self.replay_open_files(plugin_id, project_root);
                }
                Err(err) => {
                    tracing::error!(
                        "Failed to start LSP server {} for project {:?}: {:?}",
                        config.display_name,
                        project_root,
                        err
                    );
                    self.lsp_rpc.show_message(
                        format!("LSP: {}", config.display_name),
                        ShowMessageParams {
                            typ: MessageType::WARNING,
                            message: format!(
                                "Could not start {}. Is '{}' installed and on your PATH?",
                                config.display_name, config.command,
                            ),
                        },
                    );
                }
            }
            break;
        }
    }

    /// Resolve an npm-based LSP server binary, auto-installing if necessary.
    /// Returns the absolute path to the binary inside the managed directory.
    fn resolve_npm_server(
        &self,
        config: &LspServerConfig,
        package: &str,
    ) -> Result<String, String> {
        let servers_dir = Directory::lsp_servers_directory()
            .ok_or("Could not determine Lapce data directory")?;
        let prefix_dir = servers_dir.join(package);
        let bin_path = prefix_dir
            .join("node_modules")
            .join(".bin")
            .join(config.command);

        if bin_path.exists() {
            return Ok(bin_path.to_string_lossy().into_owned());
        }

        // Need to install. Find npm/node via the shell env.
        let env = self.lsp_rpc.shell_env_for_project(None);
        let npm_cmd = find_command_in_env("npm", &env).ok_or_else(|| {
            format!(
                "Could not start {}: 'npm' was not found on your PATH. \
                     Install Node.js to enable {} support.",
                config.display_name, config.display_name,
            )
        })?;

        tracing::info!("Installing {} via npm into {:?}", package, prefix_dir,);

        self.lsp_rpc.show_message(
            format!("LSP: {}", config.display_name),
            ShowMessageParams {
                typ: MessageType::INFO,
                message: format!(
                    "Installing {}... This is a one-time setup.",
                    config.display_name,
                ),
            },
        );

        let output = Command::new(&npm_cmd)
            .args(["install", "--prefix"])
            .arg(&prefix_dir)
            .arg(package)
            .envs(env.as_ref())
            .output()
            .map_err(|e| {
                format!("Failed to run npm install for {}: {}", package, e)
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "npm install {} failed: {}",
                package,
                stderr.trim(),
            ));
        }

        if bin_path.exists() {
            Ok(bin_path.to_string_lossy().into_owned())
        } else {
            Err(format!(
                "npm install {} succeeded but binary '{}' was not found at {:?}",
                package, config.command, bin_path,
            ))
        }
    }

    /// Look up the server for a given language and file path.
    fn find_server_for_path(
        &self,
        language_id: &str,
        path: Option<&std::path::Path>,
    ) -> Option<PluginId> {
        let project_root = self.effective_project_root(path);
        let key = (language_id.to_string(), project_root);
        self.language_project_to_server.get(&key).copied()
    }

    /// Replay didOpen for open files that belong to the given project root.
    fn replay_open_files(&self, plugin_id: PluginId, project_root: &PathBuf) {
        let Some(server) = self.servers.get(&plugin_id) else {
            return;
        };

        match self.lsp_rpc.proxy_rpc.get_open_files_content() {
            Ok(ProxyResponse::GetOpenFilesContentResponse { items }) => {
                for item in items {
                    let path = item.uri.to_file_path().ok();

                    // Only replay files belonging to this project root
                    let belongs = path.as_ref().is_some_and(|p| {
                        if project_root.as_os_str().is_empty() {
                            // Fallback root: only replay files not in any project
                            self.find_project_root_for_path(p).is_none()
                        } else {
                            p.starts_with(project_root)
                        }
                    });

                    if !belongs {
                        continue;
                    }

                    let language_id = Some(item.language_id.clone());
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

    /// Route a request to the appropriate server based on language_id and path.
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

        // Route by (language, project_root)
        let target_plugin_id = language_id
            .as_deref()
            .and_then(|lang| self.find_server_for_path(lang, path.as_deref()));

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

        // Route by (language, project_root)
        let target_plugin_id = language_id
            .as_deref()
            .and_then(|lang| self.find_server_for_path(lang, path.as_deref()));

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

        // Find project root for this file
        let file_path = document.uri.to_file_path().ok();
        let project_root = self.effective_project_root(file_path.as_deref());

        // Ensure a server is running for this (language, project_root)
        self.ensure_server_for_language(&document.language_id, &project_root);

        // Forward to matching server
        let key = (document.language_id.clone(), project_root);
        let target_plugin_id = self.language_project_to_server.get(&key).copied();

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
        let target_plugin_id = self.find_server_for_path(&language_id, Some(&path));

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
        let path = document.uri.to_file_path().ok();
        let target_plugin_id =
            self.find_server_for_path(&language_id, path.as_deref());

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

/// Search for an executable in the PATH from the given environment map.
fn find_command_in_env(cmd: &str, env: &HashMap<String, String>) -> Option<String> {
    let path_var = env.get("PATH")?;
    for dir in std::env::split_paths(path_var) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}
