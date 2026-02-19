use std::{
    borrow::Cow, collections::HashMap, path::PathBuf, process::Command, sync::Arc,
};

use lapce_core::directory::Directory;
use lapce_rpc::{
    RpcError, plugin::PluginId, project::ProjectInfo, proxy::ProxyResponse,
    style::LineStyle,
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
    /// How to auto-install this server if the command is not found.
    pub auto_install: AutoInstall,
}

/// Strategy for auto-installing an LSP server when its binary is not found.
pub enum AutoInstall {
    /// No auto-install. The user must install the server manually.
    None,
    /// Install via `npm install --prefix <lapce-lsp-dir>/<package> <package>`.
    /// The binary is resolved from `node_modules/.bin/<command>`.
    Npm { package: &'static str },
    /// Install via `gem install <gem>` using the project-specific shell
    /// environment (preserving Ruby version manager context).
    Gem { gem: &'static str },
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
        auto_install: AutoInstall::Gem { gem: "ruby-lsp" },
    },
    LspServerConfig {
        display_name: "bash-language-server",
        command: "bash-language-server",
        args: &["start"],
        languages: &["shellscript"],
        init_options_json: None,
        auto_install: AutoInstall::Npm {
            package: "bash-language-server",
        },
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
    /// Detected projects
    projects: Vec<ProjectInfo>,
    /// Tracks open files for lazy activation replay
    open_files: HashMap<PathBuf, TextDocumentItem>,
    /// Whether to exclude all gems from ruby-lsp indexing
    ruby_lsp_exclude_gems: bool,
    /// Additional glob patterns to exclude from ruby-lsp indexing
    ruby_lsp_excluded_patterns: Vec<String>,
}

impl LspManager {
    pub fn new(
        workspace: Option<PathBuf>,
        lsp_rpc: LspRpcHandler,
        projects: Vec<ProjectInfo>,
        ruby_lsp_exclude_gems: bool,
        ruby_lsp_excluded_patterns: Vec<String>,
    ) -> Self {
        Self {
            workspace,
            lsp_rpc,
            servers: HashMap::new(),
            language_project_to_server: HashMap::new(),
            activated_configs: Vec::new(),
            projects,
            open_files: HashMap::new(),
            ruby_lsp_exclude_gems,
            ruby_lsp_excluded_patterns,
        }
    }

    /// Find the longest matching project root that is a prefix of `path`.
    fn find_project_root_for_path(&self, path: &std::path::Path) -> Option<PathBuf> {
        self.projects
            .iter()
            .filter(|p| path.starts_with(&p.root))
            .max_by_key(|p| p.root.components().count())
            .map(|p| p.root.clone())
    }

    /// Get the effective project root for a file path, falling back to workspace.
    fn effective_project_root(&self, path: Option<&std::path::Path>) -> PathBuf {
        path.and_then(|p| self.find_project_root_for_path(p))
            .or_else(|| self.workspace.clone())
            .unwrap_or_default()
    }

    /// Build initialization options for an LSP server, merging static config
    /// with dynamic ruby-lsp indexing options when applicable.
    fn build_init_options(
        &self,
        config: &LspServerConfig,
        project_root: &std::path::Path,
    ) -> Option<Value> {
        let mut opts: Value = config
            .init_options_json
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);

        if config.command == "ruby-lsp" {
            let mut indexing = serde_json::Map::new();

            if self.ruby_lsp_exclude_gems {
                let gemfile_lock = project_root.join("Gemfile.lock");
                match parse_gemfile_lock_gems(&gemfile_lock) {
                    Ok(gems) if !gems.is_empty() => {
                        tracing::info!(
                            "ruby-lsp: excluding {} gems from indexing",
                            gems.len()
                        );
                        indexing.insert(
                            "excludedGems".to_string(),
                            Value::Array(
                                gems.into_iter().map(Value::String).collect(),
                            ),
                        );
                    }
                    Ok(_) => {
                        tracing::info!("ruby-lsp: no gems found in Gemfile.lock");
                    }
                    Err(e) => {
                        tracing::warn!(
                            "ruby-lsp: could not read Gemfile.lock: {}",
                            e
                        );
                    }
                }
            }

            if !self.ruby_lsp_excluded_patterns.is_empty() {
                indexing.insert(
                    "excludedPatterns".to_string(),
                    Value::Array(
                        self.ruby_lsp_excluded_patterns
                            .iter()
                            .cloned()
                            .map(Value::String)
                            .collect(),
                    ),
                );
            }

            if !indexing.is_empty() {
                if opts.is_null() {
                    opts = Value::Object(serde_json::Map::new());
                }
                if let Value::Object(ref mut map) = opts {
                    map.insert("indexing".to_string(), Value::Object(indexing));
                }
            }
        }

        if opts.is_null() { None } else { Some(opts) }
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

            // Use project-specific shell env (lazily resolved)
            let env = self
                .lsp_rpc
                .shell_env_for_project(Some(project_root.as_path()));

            // Enrich matching ProjectInfo entries with data from the resolved env
            let mut projects_updated = false;
            for project in &mut self.projects {
                if project.root == *project_root && project.tool_versions.is_empty()
                {
                    project.tool_versions =
                        lapce_rpc::project::extract_tool_versions(
                            &project.kind,
                            &env,
                        );
                    project.version_manager =
                        lapce_rpc::project::detect_version_manager(
                            &project.kind,
                            &env,
                        );
                    project.lsp_server = project
                        .languages
                        .first()
                        .and_then(|lang| lsp_command_for_language(lang))
                        .map(|s| s.to_string());
                    projects_updated = true;
                }
            }
            if projects_updated {
                self.lsp_rpc.projects_detected(self.projects.clone());
            }

            // Start the server with the project root as its workspace
            let server_workspace = if project_root.as_os_str().is_empty() {
                self.workspace.clone()
            } else {
                Some(project_root.clone())
            };

            let task_id = self.lsp_rpc.next_background_task_id();
            self.lsp_rpc.background_task_started(
                task_id,
                format!("Starting LSP: {}", config.display_name),
            );

            // Resolve the command. For npm-based servers, resolve from the
            // managed directory (installing if needed). For others, use the
            // command as-is.
            let resolved_command = match &config.auto_install {
                AutoInstall::Npm { package } => {
                    match self.resolve_npm_server(config, package) {
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
                            self.lsp_rpc.background_task_finished(task_id);
                            break;
                        }
                    }
                }
                _ => config.command.to_string(),
            };

            let init_options = self.build_init_options(config, project_root);

            // First attempt to start the server.
            match LspClient::start(
                self.lsp_rpc.clone(),
                server_workspace.clone(),
                config.display_name,
                &resolved_command,
                config.args,
                config.languages,
                init_options.clone(),
                env.clone(),
            ) {
                Ok(client) => {
                    self.register_server(
                        client,
                        activation_key,
                        config,
                        project_root,
                    );
                }
                Err(first_err) => {
                    // For gem-based servers, try auto-installing then retry.
                    if let AutoInstall::Gem { gem } = &config.auto_install {
                        if self.try_gem_install(config, gem, &env).is_ok() {
                            // Retry after install
                            if let Ok(client) = LspClient::start(
                                self.lsp_rpc.clone(),
                                server_workspace,
                                config.display_name,
                                &resolved_command,
                                config.args,
                                config.languages,
                                init_options,
                                env,
                            ) {
                                self.register_server(
                                    client,
                                    activation_key,
                                    config,
                                    project_root,
                                );
                                self.lsp_rpc.background_task_finished(task_id);
                                break;
                            }
                        }
                    }

                    tracing::error!(
                        "Failed to start LSP server {} for project {:?}: {:?}",
                        config.display_name,
                        project_root,
                        first_err,
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
            self.lsp_rpc.background_task_finished(task_id);
            break;
        }
    }

    /// Register a successfully started LSP client.
    fn register_server(
        &mut self,
        client: LspClient,
        activation_key: (usize, PathBuf),
        config: &LspServerConfig,
        project_root: &PathBuf,
    ) {
        let plugin_id = client.plugin_id;
        self.activated_configs.push(activation_key);

        for lang in config.languages {
            self.language_project_to_server
                .insert((lang.to_string(), project_root.clone()), plugin_id);
        }

        self.servers.insert(plugin_id, client);
        self.replay_open_files(plugin_id, project_root);
    }

    /// Try to install a gem using the given environment. Returns Ok(()) on
    /// success, Err with a message on failure.
    fn try_gem_install(
        &self,
        config: &LspServerConfig,
        gem: &str,
        env: &Arc<HashMap<String, String>>,
    ) -> Result<(), String> {
        let gem_cmd = find_command_in_env("gem", env).ok_or_else(|| {
            format!(
                "Could not install {}: 'gem' was not found on your PATH.",
                config.display_name,
            )
        })?;

        let task_id = self.lsp_rpc.next_background_task_id();
        self.lsp_rpc
            .background_task_started(task_id, format!("Installing gem: {gem}"));

        tracing::info!("Installing gem {} for {}", gem, config.display_name);

        let output = Command::new(&gem_cmd)
            .args(["install", gem])
            .envs(env.as_ref())
            .output()
            .map_err(|e| {
                self.lsp_rpc.background_task_finished(task_id);
                format!("Failed to run gem install {}: {}", gem, e)
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = format!("gem install {} failed: {}", gem, stderr.trim());
            tracing::error!("{}", msg);
            self.lsp_rpc.show_message(
                format!("LSP: {}", config.display_name),
                ShowMessageParams {
                    typ: MessageType::WARNING,
                    message: msg.clone(),
                },
            );
            self.lsp_rpc.background_task_finished(task_id);
            return Err(msg);
        }

        tracing::info!("Successfully installed gem {}", gem);
        self.lsp_rpc.background_task_finished(task_id);
        Ok(())
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

        let task_id = self.lsp_rpc.next_background_task_id();
        self.lsp_rpc
            .background_task_started(task_id, format!("Installing {package}"));

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
                self.lsp_rpc.background_task_finished(task_id);
                format!("Failed to run npm install for {}: {}", package, e)
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            self.lsp_rpc.background_task_finished(task_id);
            return Err(format!(
                "npm install {} failed: {}",
                package,
                stderr.trim(),
            ));
        }

        self.lsp_rpc.background_task_finished(task_id);
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

/// Parse gem names from a Gemfile.lock file. Only extracts gems from the
/// `GEM` section's `specs:` block (the main RubyGems source). Gems from
/// `GIT` or `PATH` sections are excluded — those are local/development gems
/// that ruby-lsp should still index.
pub fn parse_gemfile_lock_gems(
    path: &std::path::Path,
) -> Result<Vec<String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let mut gems = Vec::new();
    let mut in_gem_section = false;
    let mut in_specs = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Top-level section headers have no leading whitespace
        if !line.starts_with(' ') && !trimmed.is_empty() {
            in_gem_section = trimmed == "GEM";
            in_specs = false;
            continue;
        }

        if !in_gem_section {
            continue;
        }

        if trimmed == "specs:" {
            in_specs = true;
            continue;
        }

        if !in_specs {
            continue;
        }

        // Gem entries in the specs block have exactly 4 spaces of indent:
        //     gem_name (version)
        // Sub-dependencies have 6+ spaces. We only want top-level gems.
        if line.starts_with("    ") && !line.starts_with("      ") {
            if let Some(name) = trimmed.split_whitespace().next() {
                gems.push(name.to_string());
            }
        }
    }

    Ok(gems)
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

#[cfg(test)]
mod tests {
    use super::parse_gemfile_lock_gems;

    #[test]
    fn extracts_gems_from_gem_section() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("Gemfile.lock");
        std::fs::write(
            &lock,
            "\
GEM
  remote: https://rubygems.org/
  specs:
    actioncable (7.1.3)
      actionpack (= 7.1.3)
    actionpack (7.1.3)
      rack (>= 2.2.4)
    rails (7.1.3)
      actioncable (= 7.1.3)

PLATFORMS
  ruby

DEPENDENCIES
  rails (~> 7.1)
",
        )
        .unwrap();

        let gems = parse_gemfile_lock_gems(&lock).unwrap();
        assert_eq!(gems, vec!["actioncable", "actionpack", "rails"]);
    }

    #[test]
    fn does_not_extract_from_git_or_path_sections() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("Gemfile.lock");
        std::fs::write(
            &lock,
            "\
GIT
  remote: https://github.com/example/foo.git
  revision: abc123
  specs:
    foo (1.0.0)

PATH
  remote: vendor/bar
  specs:
    bar (0.1.0)

GEM
  remote: https://rubygems.org/
  specs:
    rails (7.1.3)

PLATFORMS
  ruby
",
        )
        .unwrap();

        let gems = parse_gemfile_lock_gems(&lock).unwrap();
        assert_eq!(gems, vec!["rails"]);
    }

    #[test]
    fn returns_error_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("nonexistent");
        assert!(parse_gemfile_lock_gems(&lock).is_err());
    }

    #[test]
    fn returns_empty_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("Gemfile.lock");
        std::fs::write(&lock, "").unwrap();

        let gems = parse_gemfile_lock_gems(&lock).unwrap();
        assert!(gems.is_empty());
    }
}
