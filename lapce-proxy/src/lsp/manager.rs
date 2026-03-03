use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
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

use dyn_clone::clone_box;

use super::{ClonableCallback, LspRpcHandler, RpcCallback, client::LspClient};
use crate::shell_env::find_command_in_env;

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
    /// Condition that must be met for this server to activate.
    pub activation_condition: ActivationCondition,
    /// When true, each sub-project gets its own server instance even if a
    /// parent project exists for the same language. Required for servers
    /// like vtsls where each sub-project has its own tsconfig.json and
    /// a single instance at the monorepo root would be too large.
    /// When false (default for most servers), a parent project's server
    /// covers child sub-projects.
    pub per_project_instance: bool,
    /// JSON string for workspace/configuration settings, returned when
    /// the server sends a `workspace/configuration` request.
    /// Structure follows VS Code settings format with top-level sections
    /// (e.g. `{"typescript": {...}, "vtsls": {...}}`).
    pub settings_json: Option<&'static str>,
    /// When true, discard the server's semantic tokens capability so that
    /// tree-sitter highlighting is used instead.
    pub semantic_tokens: bool,
    /// Optional callback to enrich settings with dynamic values computed
    /// from the workspace. Called after parsing `settings_json`.
    pub compute_settings: Option<fn(&mut Value, Option<&Path>)>,
}

/// Condition that must be met for a server to activate.
pub enum ActivationCondition {
    /// Always activate when a matching file is opened.
    Always,
    /// Only activate if the given gem is present in the project's Gemfile.lock.
    GemInGemfileLock(&'static str),
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
        activation_condition: ActivationCondition::Always,
        per_project_instance: false,
        settings_json: None,
        semantic_tokens: false,
        compute_settings: None,
    },
    LspServerConfig {
        display_name: "sorbet",
        command: "srb",
        args: &["tc", "--lsp"],
        languages: &["ruby"],
        init_options_json: None,
        auto_install: AutoInstall::None,
        activation_condition: ActivationCondition::GemInGemfileLock("sorbet-static"),
        per_project_instance: false,
        settings_json: None,
        semantic_tokens: false,
        compute_settings: None,
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
        activation_condition: ActivationCondition::Always,
        per_project_instance: false,
        settings_json: None,
        semantic_tokens: false,
        compute_settings: None,
    },
    LspServerConfig {
        display_name: "vtsls",
        command: "vtsls",
        args: &["--stdio"],
        languages: &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
        init_options_json: None,
        auto_install: AutoInstall::Npm {
            package: "@vtsls/language-server",
        },
        activation_condition: ActivationCondition::Always,
        per_project_instance: false,
        // Settings returned via workspace/configuration (VS Code format).
        // vtsls ignores initializationOptions for these; they must come here.
        // Note: maxTsServerMemory is computed dynamically based on the number
        // of ts/tsx/js/jsx files in the project (see compute_settings).
        settings_json: Some(
            r#"{
                "vtsls": {
                    "autoUseWorkspaceTsdk": true
                }
            }"#,
        ),
        semantic_tokens: false,
        compute_settings: Some(compute_vtsls_settings),
    },
    LspServerConfig {
        display_name: "eslint",
        command: "vscode-eslint-language-server",
        args: &["--stdio"],
        languages: &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
        init_options_json: None,
        auto_install: AutoInstall::Npm {
            package: "vscode-langservers-extracted",
        },
        activation_condition: ActivationCondition::Always,
        per_project_instance: false,
        // Settings are flat (not nested under "eslint") because the server
        // requests workspace/configuration with section="" and reads fields
        // like `validate`, `experimental`, etc. from the top level.
        settings_json: Some(
            r#"{
                "validate": "on",
                "run": "onType",
                "quiet": false,
                "onIgnoredFiles": "off",
                "rulesCustomizations": [],
                "problems": { "shortenToSingleLine": false },
                "nodePath": "",
                "experimental": {},
                "codeAction": {
                    "disableRuleComment": { "enable": true, "location": "separateLine" },
                    "showDocumentation": { "enable": true }
                }
            }"#,
        ),
        semantic_tokens: false,
        compute_settings: Some(compute_eslint_settings),
    },
];

/// Find the LSP server command configured for a given language, if any.
pub fn lsp_command_for_language(language_id: &str) -> Option<&'static str> {
    LSP_SERVERS
        .iter()
        .find(|c| c.languages.contains(&language_id))
        .map(|c| c.command)
}

/// Find all LSP server display names configured for a given language.
pub fn lsp_servers_for_language(language_id: &str) -> Vec<&'static str> {
    LSP_SERVERS
        .iter()
        .filter(|c| c.languages.contains(&language_id))
        .map(|c| c.display_name)
        .collect()
}

/// Count ts/tsx/js/jsx source files under `dir`, excluding `node_modules`.
/// Uses a simple directory walk to avoid external dependencies.
fn count_ts_js_files(dir: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if file_type.is_dir() {
                if name == "node_modules" || name.starts_with('.') {
                    continue;
                }
                stack.push(entry.path());
            } else if file_type.is_file() {
                if name.ends_with(".ts")
                    || name.ends_with(".tsx")
                    || name.ends_with(".js")
                    || name.ends_with(".jsx")
                {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Compute `maxTsServerMemory` (in MB) based on the number of source files.
///
/// Heuristic:
///   base = 512 MB (minimum for tsserver to run comfortably)
///   per_file = 0.5 MB per source file
///   buffer = 1.5x multiplier (for node_modules type definitions we can't count)
///   result = clamp(1024..=8192, (base + file_count * 0.5) * 1.5)
fn compute_max_ts_server_memory(file_count: usize) -> u64 {
    let raw = (512.0 + file_count as f64 * 0.5) * 1.5;
    (raw as u64).clamp(1024, 8192)
}

/// Compute dynamic vtsls settings: inject maxTsServerMemory based on file count.
fn compute_vtsls_settings(settings: &mut Value, workspace: Option<&Path>) {
    let file_count = workspace.map(count_ts_js_files).unwrap_or(0);
    let memory = compute_max_ts_server_memory(file_count);
    tracing::info!(
        "vtsls: counted {file_count} ts/js files, setting maxTsServerMemory={memory} MB"
    );

    if let Value::Object(map) = settings {
        let ts = map
            .entry("typescript")
            .or_insert_with(|| Value::Object(Default::default()));
        if let Value::Object(ts_map) = ts {
            let tsserver = ts_map
                .entry("tsserver")
                .or_insert_with(|| Value::Object(Default::default()));
            if let Value::Object(tsserver_map) = tsserver {
                tsserver_map.insert(
                    "maxTsServerMemory".to_string(),
                    Value::Number(memory.into()),
                );
            }
        }
    }
}

/// Compute dynamic eslint settings: inject workspaceFolder, workingDirectory,
/// and detect flat config.
fn compute_eslint_settings(settings: &mut Value, workspace: Option<&Path>) {
    let Some(ws) = workspace else { return };
    let Value::Object(map) = settings else { return };

    // workspaceFolder at root level — the server uses this to
    // determine how far to traverse up the filesystem for config.
    let uri = lsp_types::Url::from_directory_path(ws)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", ws.display()));
    let name = ws
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    map.insert(
        "workspaceFolder".to_string(),
        serde_json::json!({ "uri": uri, "name": name }),
    );

    // workingDirectory at root level — tells the server how
    // to resolve the cwd for eslint execution.
    map.insert(
        "workingDirectory".to_string(),
        serde_json::json!({ "mode": "auto" }),
    );

    // Detect flat config (eslint.config.{js,mjs,cjs,ts,mts,cts})
    let flat_config_names = [
        "eslint.config.js",
        "eslint.config.mjs",
        "eslint.config.cjs",
        "eslint.config.ts",
        "eslint.config.mts",
        "eslint.config.cts",
    ];
    let has_flat_config = flat_config_names.iter().any(|f| ws.join(f).exists());
    if has_flat_config {
        map.insert(
            "experimental".to_string(),
            serde_json::json!({ "useFlatConfig": true }),
        );
        tracing::info!("eslint: detected flat config in {}", ws.display());
    }
}

/// Build the final settings JSON for a server, enriching the static
/// `settings_json` with any dynamic values computed from the workspace.
pub fn compute_server_settings(
    config: &LspServerConfig,
    workspace: Option<&Path>,
) -> Option<String> {
    let base_json = config.settings_json?;
    let mut settings: Value =
        serde_json::from_str(base_json).unwrap_or(Value::Object(Default::default()));

    if let Some(compute) = config.compute_settings {
        compute(&mut settings, workspace);
    }

    Some(settings.to_string())
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
    /// Maps (language_id, project_root) → Vec<PluginId> for multi-server routing
    language_project_to_server: HashMap<(String, PathBuf), Vec<PluginId>>,
    /// Tracks which (config_index, project_root) pairs have been activated
    activated_configs: HashSet<(usize, PathBuf)>,
    /// Tracks (config_index, project_root) pairs that have a background install in progress
    pending_installs: HashSet<(usize, PathBuf)>,
    /// Detected projects
    projects: Vec<ProjectInfo>,
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
            activated_configs: HashSet::new(),
            pending_installs: HashSet::new(),
            projects,
            ruby_lsp_exclude_gems,
            ruby_lsp_excluded_patterns,
        }
    }

    /// Find the project root for a file, discovering it lazily if not yet known.
    ///
    /// First checks cached projects, then walks up from the file looking for
    /// marker files. Newly discovered projects are cached and sent to the UI.
    fn find_or_discover_project_root(
        &mut self,
        path: &std::path::Path,
    ) -> Option<PathBuf> {
        // Check already-known projects first (O(n) but n is small)
        if let Some(root) = self
            .projects
            .iter()
            .filter(|p| path.starts_with(&p.root))
            .max_by_key(|p| p.root.components().count())
            .map(|p| p.root.clone())
        {
            return Some(root);
        }

        // Walk up from the file to discover a new project
        let mut project =
            crate::project::find_project_for_file(path, self.workspace.as_deref())?;

        // Populate lsp_servers from static config
        project.lsp_servers = project
            .languages
            .first()
            .map(|lang| {
                lsp_servers_for_language(lang)
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let root = project.root.clone();
        self.projects.push(project);
        // Sort by depth (shallowest first) for consistent ordering
        self.projects.sort_by_key(|p| p.root.components().count());
        self.lsp_rpc.projects_detected(self.projects.clone());

        Some(root)
    }

    /// Get the effective project root for a file path, falling back to workspace.
    fn effective_project_root(&mut self, path: Option<&std::path::Path>) -> PathBuf {
        path.and_then(|p| self.find_or_discover_project_root(p))
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

    /// Try to activate all matching servers for the given language and project
    /// root. Multiple configs can match (e.g. ruby-lsp + sorbet for Ruby).
    pub fn ensure_server_for_language(
        &mut self,
        language_id: &str,
        project_root: &PathBuf,
    ) {
        // Iterate ALL configs — don't break early, we may need multiple servers
        for (idx, config) in LSP_SERVERS.iter().enumerate() {
            let activation_key = (idx, project_root.clone());
            if self.activated_configs.contains(&activation_key) {
                continue;
            }
            // Clear pending install flag on retry so the server can activate
            self.pending_installs.remove(&activation_key);
            if !config.languages.contains(&language_id) {
                continue;
            }

            // For servers that don't use per_project_instance, skip if a
            // parent project exists for the same language — the parent's
            // server covers child directories (e.g., Ruby monorepos with
            // one Gemfile at the root).
            // For per_project_instance servers (e.g., vtsls), each
            // sub-project gets its own instance because they may have
            // independent configs (tsconfig.json) and a single instance at
            // the monorepo root can crash on large projects.
            if !config.per_project_instance {
                let parent_root = self
                    .projects
                    .iter()
                    .filter(|p| {
                        !p.root.as_os_str().is_empty()
                            && project_root.starts_with(&p.root)
                            && p.root != *project_root
                            && p.languages
                                .iter()
                                .any(|l| config.languages.contains(&l.as_str()))
                    })
                    .max_by_key(|p| p.root.components().count())
                    .map(|p| p.root.clone());
                if let Some(parent_root) = parent_root {
                    self.activated_configs.insert(activation_key);
                    // Ensure the parent project's server is started so it
                    // can cover this child project's files.
                    self.ensure_server_for_language(language_id, &parent_root);
                    continue;
                }
            }

            // Check activation condition
            match &config.activation_condition {
                ActivationCondition::Always => {}
                ActivationCondition::GemInGemfileLock(gem) => {
                    if !gemfile_lock_contains_gem(project_root, gem) {
                        // Mark as activated so we don't re-check on every file open
                        self.activated_configs.insert(activation_key);
                        continue;
                    }
                }
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
                    project.lsp_servers = project
                        .languages
                        .first()
                        .map(|lang| {
                            lsp_servers_for_language(lang)
                                .into_iter()
                                .map(|s| s.to_string())
                                .collect()
                        })
                        .unwrap_or_default();
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
                    match self.resolve_npm_server_or_install_async(
                        config,
                        package,
                        language_id,
                        project_root,
                        &activation_key,
                    ) {
                        Ok(path) => path,
                        Err(_) => {
                            // Install is running in background, or failed.
                            // Will retry via RetryServerActivation when done.
                            self.lsp_rpc.background_task_finished(task_id);
                            continue;
                        }
                    }
                }
                _ => config.command.to_string(),
            };

            let init_options = self.build_init_options(config, project_root);
            let settings =
                compute_server_settings(config, server_workspace.as_deref());

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
                config.semantic_tokens,
                settings.clone(),
            ) {
                Ok(client) => {
                    self.register_server(
                        client,
                        activation_key,
                        config,
                        project_root,
                        env.clone(),
                    );
                }
                Err(first_err) => {
                    // For gem-based servers, spawn a background install thread
                    // so we don't block the LSP manager from processing other messages.
                    if let AutoInstall::Gem { gem } = &config.auto_install {
                        if !self.pending_installs.contains(&activation_key) {
                            self.pending_installs.insert(activation_key.clone());
                            let lsp_rpc = self.lsp_rpc.clone();
                            let gem = gem.to_string();
                            let display_name = config.display_name.to_string();
                            let env = env.clone();
                            let language_id = language_id.to_string();
                            let project_root = project_root.clone();
                            std::thread::spawn(move || {
                                let gem_cmd = match find_command_in_env("gem", &env)
                                {
                                    Some(cmd) => cmd,
                                    None => return,
                                };
                                let task_id = lsp_rpc.next_background_task_id();
                                lsp_rpc.background_task_started(
                                    task_id,
                                    format!("Installing gem: {gem}"),
                                );
                                tracing::info!("Background installing gem {}", gem);
                                match Command::new(&gem_cmd)
                                    .args(["install", &gem])
                                    .envs(env.as_ref())
                                    .output()
                                {
                                    Ok(output) if output.status.success() => {
                                        tracing::info!(
                                            "Successfully installed gem {}",
                                            gem
                                        );
                                        lsp_rpc.background_task_finished(task_id);
                                        // Notify LSP manager to retry
                                        lsp_rpc.retry_server_activation(
                                            language_id,
                                            project_root,
                                        );
                                    }
                                    Ok(output) => {
                                        let stderr =
                                            String::from_utf8_lossy(&output.stderr);
                                        tracing::error!(
                                            "gem install {} failed: {}",
                                            gem,
                                            stderr.trim()
                                        );
                                        lsp_rpc.show_message(
                                            format!("LSP: {display_name}"),
                                            ShowMessageParams {
                                                typ: MessageType::WARNING,
                                                message: format!(
                                                    "gem install {gem} failed: {}",
                                                    stderr.trim()
                                                ),
                                            },
                                        );
                                        lsp_rpc.background_task_finished(task_id);
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to run gem install: {e}"
                                        );
                                        lsp_rpc.background_task_finished(task_id);
                                    }
                                }
                            });
                        }
                    } else {
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
            }
            self.lsp_rpc.background_task_finished(task_id);
        }
    }

    /// Register a successfully started LSP client.
    fn register_server(
        &mut self,
        client: LspClient,
        activation_key: (usize, PathBuf),
        config: &LspServerConfig,
        project_root: &PathBuf,
        env: Arc<HashMap<String, String>>,
    ) {
        let plugin_id = client.plugin_id;
        self.activated_configs.insert(activation_key);

        for lang in config.languages {
            self.language_project_to_server
                .entry((lang.to_string(), project_root.clone()))
                .or_default()
                .push(plugin_id);
        }

        self.servers.insert(plugin_id, client);
        self.replay_open_files(plugin_id, project_root);

        // Spawn a background update if enough time has passed since the last one.
        self.maybe_spawn_background_update(config, env);
    }

    /// If the server has an auto-install type and we haven't updated recently,
    /// spawn a background thread to update the package to the latest version.
    fn maybe_spawn_background_update(
        &self,
        config: &LspServerConfig,
        env: Arc<HashMap<String, String>>,
    ) {
        match &config.auto_install {
            AutoInstall::Gem { gem } => {
                if should_check_update(gem) {
                    // Touch immediately so concurrent activations (e.g.
                    // per_project_instance servers) don't spawn duplicates.
                    touch_update_marker(gem);
                    self.spawn_background_update_gem(gem, env);
                }
            }
            AutoInstall::Npm { package } => {
                if should_check_update(package) {
                    touch_update_marker(package);
                    self.spawn_background_update_npm(package);
                }
            }
            AutoInstall::None => {}
        }
    }

    /// Spawn a background thread to update a gem-based LSP server.
    fn spawn_background_update_gem(
        &self,
        gem: &str,
        env: Arc<HashMap<String, String>>,
    ) {
        let lsp_rpc = self.lsp_rpc.clone();
        let gem = gem.to_string();
        std::thread::spawn(move || {
            let gem_cmd = match find_command_in_env("gem", &env) {
                Some(cmd) => cmd,
                None => return,
            };
            let task_id = lsp_rpc.next_background_task_id();
            lsp_rpc.background_task_started(task_id, format!("Updating {gem}"));
            tracing::info!("Background updating gem {}", gem);
            match Command::new(&gem_cmd)
                .args(["install", &gem])
                .envs(env.as_ref())
                .output()
            {
                Ok(output) if output.status.success() => {
                    tracing::info!("Successfully updated gem {}", gem);
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::error!("gem update {} failed: {}", gem, stderr.trim());
                }
                Err(e) => {
                    tracing::error!("Failed to run gem update: {e}");
                }
            }
            lsp_rpc.background_task_finished(task_id);
        });
    }

    /// Spawn a background thread to update an npm-based LSP server.
    fn spawn_background_update_npm(&self, package: &str) {
        let lsp_rpc = self.lsp_rpc.clone();
        let package = package.to_string();
        std::thread::spawn(move || {
            let env = lsp_rpc.shell_env_for_project(None);
            let npm_cmd = match find_command_in_env("npm", &env) {
                Some(cmd) => cmd,
                None => return,
            };
            let servers_dir = match Directory::lsp_servers_directory() {
                Some(dir) => dir,
                None => return,
            };
            let prefix_dir = servers_dir.join(&package);
            let task_id = lsp_rpc.next_background_task_id();
            lsp_rpc.background_task_started(task_id, format!("Updating {package}"));
            tracing::info!("Background updating npm package {}", package);
            match Command::new(&npm_cmd)
                .args(["install", "--prefix"])
                .arg(&prefix_dir)
                .arg(format!("{}@latest", package))
                .envs(env.as_ref())
                .output()
            {
                Ok(output) if output.status.success() => {
                    tracing::info!("Successfully updated npm package {}", package);
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::error!(
                        "npm update {} failed: {}",
                        package,
                        stderr.trim()
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to run npm update: {e}");
                }
            }
            lsp_rpc.background_task_finished(task_id);
        });
    }

    /// Check if the npm server binary exists. If not, spawn a background
    /// install thread and return Err. The thread will send RetryServerActivation
    /// when done.
    fn resolve_npm_server_or_install_async(
        &mut self,
        config: &LspServerConfig,
        package: &str,
        language_id: &str,
        project_root: &PathBuf,
        activation_key: &(usize, PathBuf),
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

        // Already installing — don't spawn another thread
        if self.pending_installs.contains(activation_key) {
            return Err("install in progress".to_string());
        }

        self.pending_installs.insert(activation_key.clone());
        let lsp_rpc = self.lsp_rpc.clone();
        let package = package.to_string();
        let display_name = config.display_name.to_string();
        let command = config.command.to_string();
        let language_id = language_id.to_string();
        let project_root = project_root.clone();

        std::thread::spawn(move || {
            let env = lsp_rpc.shell_env_for_project(None);
            let npm_cmd = match find_command_in_env("npm", &env) {
                Some(cmd) => cmd,
                None => {
                    lsp_rpc.show_message(
                        format!("LSP: {display_name}"),
                        ShowMessageParams {
                            typ: MessageType::WARNING,
                            message: format!(
                                "Could not start {display_name}: 'npm' was not found on your PATH."
                            ),
                        },
                    );
                    return;
                }
            };

            let task_id = lsp_rpc.next_background_task_id();
            lsp_rpc
                .background_task_started(task_id, format!("Installing {package}"));
            lsp_rpc.show_message(
                format!("LSP: {display_name}"),
                ShowMessageParams {
                    typ: MessageType::INFO,
                    message: format!(
                        "Installing {display_name}... This is a one-time setup."
                    ),
                },
            );

            tracing::info!(
                "Background installing {} via npm into {:?}",
                package,
                prefix_dir
            );
            match Command::new(&npm_cmd)
                .args(["install", "--prefix"])
                .arg(&prefix_dir)
                .arg(&package)
                .envs(env.as_ref())
                .output()
            {
                Ok(output) if output.status.success() => {
                    let expected_bin =
                        prefix_dir.join("node_modules").join(".bin").join(&command);
                    if expected_bin.exists() {
                        tracing::info!("Successfully installed {}", package);
                        lsp_rpc.background_task_finished(task_id);
                        lsp_rpc.retry_server_activation(language_id, project_root);
                    } else {
                        tracing::error!(
                            "npm install succeeded but binary not found: {:?}",
                            expected_bin
                        );
                        lsp_rpc.background_task_finished(task_id);
                    }
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::error!(
                        "npm install {} failed: {}",
                        package,
                        stderr.trim()
                    );
                    lsp_rpc.show_message(
                        format!("LSP: {display_name}"),
                        ShowMessageParams {
                            typ: MessageType::WARNING,
                            message: format!(
                                "npm install {package} failed: {}",
                                stderr.trim()
                            ),
                        },
                    );
                    lsp_rpc.background_task_finished(task_id);
                }
                Err(e) => {
                    tracing::error!("Failed to run npm install: {e}");
                    lsp_rpc.background_task_finished(task_id);
                }
            }
        });

        Err("install started in background".to_string())
    }

    /// Look up all servers for a given language and file path.
    /// Falls back to a parent project root's server when the deepest match
    /// was skipped (covered by parent).
    fn find_servers_for_path(
        &mut self,
        language_id: &str,
        path: Option<&std::path::Path>,
    ) -> Vec<PluginId> {
        let project_root = self.effective_project_root(path);
        let key = (language_id.to_string(), project_root.clone());
        if let Some(ids) = self.language_project_to_server.get(&key) {
            if !ids.is_empty() {
                return ids.clone();
            }
        }
        // No server for the exact project root — find a parent project root
        // that has a server for this language.
        let lang = language_id.to_string();
        self.language_project_to_server
            .iter()
            .filter(|((l, root), ids)| {
                l == &lang
                    && !ids.is_empty()
                    && !root.as_os_str().is_empty()
                    && project_root.starts_with(root)
            })
            .max_by_key(|((_, root), _)| root.components().count())
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default()
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
                            !self
                                .projects
                                .iter()
                                .any(|proj| p.starts_with(&proj.root))
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

    /// Route a request to the appropriate server(s) based on language_id and
    /// path. When multiple servers match, fan-out to all and merge results.
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

        // Route by (language, project_root) — may have multiple servers
        let server_ids = language_id
            .as_deref()
            .map(|lang| self.find_servers_for_path(lang, path.as_deref()))
            .unwrap_or_default();

        if server_ids.is_empty() {
            f(
                PluginId(0),
                Err(RpcError::new("no LSP server available for this language")),
            );
            return;
        }

        // Single server — fast path (no merge needed)
        if server_ids.len() == 1 {
            let pid = server_ids[0];
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
            }
            return;
        }

        // Multiple servers — fan-out and merge
        let total = server_ids.len();
        let state = Arc::new(Mutex::new(MultiServerState {
            remaining: total,
            results: Vec::with_capacity(total),
        }));

        for pid in server_ids {
            let state = Arc::clone(&state);
            let f = clone_box(&*f);
            if let Some(server) = self.servers.get(&pid) {
                server.server_request_async(
                    method.clone(),
                    params.clone(),
                    language_id.clone(),
                    path.clone(),
                    move |result| {
                        let mut guard = state.lock().unwrap();
                        guard.results.push((pid, result));
                        guard.remaining -= 1;
                        if guard.remaining == 0 {
                            let (merged_pid, merged_result) = merge_server_results(
                                std::mem::take(&mut guard.results),
                            );
                            f(merged_pid, merged_result);
                        }
                    },
                );
            } else {
                let mut guard = state.lock().unwrap();
                guard.remaining -= 1;
                if guard.remaining == 0 {
                    let (merged_pid, merged_result) =
                        merge_server_results(std::mem::take(&mut guard.results));
                    f(merged_pid, merged_result);
                }
            }
        }
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

        // Broadcast to all servers for this (language, project_root)
        let server_ids = language_id
            .as_deref()
            .map(|lang| self.find_servers_for_path(lang, path.as_deref()))
            .unwrap_or_default();

        for pid in server_ids {
            if let Some(server) = self.servers.get(&pid) {
                server.server_notification(
                    method.clone(),
                    params.clone(),
                    language_id.clone(),
                    path.clone(),
                );
            }
        }
    }

    pub fn handle_did_open_text_document(&mut self, document: TextDocumentItem) {
        // Find project root for this file
        let file_path = document.uri.to_file_path().ok();
        let project_root = self.effective_project_root(file_path.as_deref());

        // Ensure servers are running for this (language, project_root)
        self.ensure_server_for_language(&document.language_id, &project_root);

        // Broadcast to all matching servers (falls back to parent project
        // server when this file's project root was covered by a parent).
        let server_ids =
            self.find_servers_for_path(&document.language_id, file_path.as_deref());

        for pid in server_ids {
            let path = document.uri.to_file_path().ok();
            if let Some(server) = self.servers.get(&pid) {
                server.server_notification(
                    DidOpenTextDocument::METHOD,
                    DidOpenTextDocumentParams {
                        text_document: document.clone(),
                    },
                    Some(document.language_id.clone()),
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
        let server_ids = self.find_servers_for_path(&language_id, Some(&path));

        for pid in server_ids {
            if let Some(server) = self.servers.get(&pid) {
                server.handle_did_save_text_document(
                    language_id.clone(),
                    path.clone(),
                    text_document.clone(),
                    text.clone(),
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
        let server_ids = self.find_servers_for_path(&language_id, path.as_deref());

        for pid in server_ids {
            if let Some(server) = self.servers.get_mut(&pid) {
                server.handle_did_change_text_document(
                    language_id.clone(),
                    document.clone(),
                    delta.clone(),
                    text.clone(),
                    new_text.clone(),
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

/// Check whether a gem is listed in the project's Gemfile.lock.
fn gemfile_lock_contains_gem(
    project_root: &std::path::Path,
    gem_name: &str,
) -> bool {
    let gemfile_lock = project_root.join("Gemfile.lock");
    parse_gemfile_lock_gems(&gemfile_lock)
        .map(|gems| gems.iter().any(|g| g == gem_name))
        .unwrap_or(false)
}

/// Accumulator for multi-server fan-out responses.
struct MultiServerState {
    remaining: usize,
    results: Vec<(PluginId, Result<Value, RpcError>)>,
}

/// Merge results from multiple LSP servers into a single response.
///
/// Strategy:
/// - If all servers errored → return the last error
/// - Array values → concatenate all arrays
/// - Null from one + non-null from another → use non-null
/// - Multiple non-null non-array → use first
fn merge_server_results(
    results: Vec<(PluginId, Result<Value, RpcError>)>,
) -> (PluginId, Result<Value, RpcError>) {
    let mut successes: Vec<(PluginId, Value)> = Vec::new();
    let mut last_error: Option<(PluginId, RpcError)> = None;

    for (pid, result) in results {
        match result {
            Ok(value) => successes.push((pid, value)),
            Err(err) => last_error = Some((pid, err)),
        }
    }

    if successes.is_empty() {
        return last_error
            .map(|(pid, err)| (pid, Err(err)))
            .unwrap_or_else(|| {
                (PluginId(0), Err(RpcError::new("no server responses")))
            });
    }

    let first_pid = successes[0].0;

    // Check if any success is an array — if so, concatenate all arrays
    let has_array = successes.iter().any(|(_, v)| v.is_array());

    if has_array {
        let mut merged = Vec::new();
        for (_, value) in successes {
            if let Value::Array(arr) = value {
                merged.extend(arr);
            }
            // Skip nulls and non-arrays when merging arrays
        }
        return (first_pid, Ok(Value::Array(merged)));
    }

    // No arrays — pick the first non-null value
    for (pid, value) in &successes {
        if !value.is_null() {
            return (*pid, Ok(value.clone()));
        }
    }

    // All nulls
    (first_pid, Ok(Value::Null))
}

/// How often to check for LSP server updates (24 hours).
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Check whether we should run an update for the named server by inspecting
/// the mtime of its marker file. Returns true if the marker doesn't exist or
/// is older than `UPDATE_CHECK_INTERVAL`.
fn should_check_update(name: &str) -> bool {
    let markers_dir = match Directory::lsp_update_markers_directory() {
        Some(dir) => dir,
        None => return false,
    };
    let marker = markers_dir.join(name);
    match marker.metadata().and_then(|m| m.modified()) {
        Ok(mtime) => {
            SystemTime::now()
                .duration_since(mtime)
                .unwrap_or(Duration::ZERO)
                > UPDATE_CHECK_INTERVAL
        }
        Err(_) => true, // marker doesn't exist
    }
}

/// Create or touch the update marker file for the named server.
fn touch_update_marker(name: &str) {
    let Some(markers_dir) = Directory::lsp_update_markers_directory() else {
        return;
    };
    let marker = markers_dir.join(name);
    if let Err(e) = std::fs::File::create(&marker) {
        tracing::error!("Failed to touch update marker {:?}: {e}", marker);
    }
}

#[cfg(test)]
mod tests {
    use super::{merge_server_results, parse_gemfile_lock_gems};
    use lapce_rpc::{RpcError, plugin::PluginId};
    use serde_json::Value;

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

    #[test]
    fn gemfile_lock_contains_gem_returns_true_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("Gemfile.lock");
        std::fs::write(
            &lock,
            "\
GEM
  remote: https://rubygems.org/
  specs:
    sorbet-static (0.5.11000)
    sorbet (0.5.11000)
      sorbet-static (= 0.5.11000)

PLATFORMS
  ruby
",
        )
        .unwrap();

        assert!(super::gemfile_lock_contains_gem(
            dir.path(),
            "sorbet-static"
        ));
    }

    #[test]
    fn gemfile_lock_contains_gem_returns_false_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("Gemfile.lock");
        std::fs::write(
            &lock,
            "\
GEM
  remote: https://rubygems.org/
  specs:
    rails (7.1.3)

PLATFORMS
  ruby
",
        )
        .unwrap();

        assert!(!super::gemfile_lock_contains_gem(
            dir.path(),
            "sorbet-static"
        ));
    }

    #[test]
    fn gemfile_lock_contains_gem_returns_false_when_no_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!super::gemfile_lock_contains_gem(
            dir.path(),
            "sorbet-static"
        ));
    }

    #[test]
    fn merge_concatenates_arrays() {
        let results = vec![
            (
                PluginId(1),
                Ok(Value::Array(vec![Value::String("a".into())])),
            ),
            (
                PluginId(2),
                Ok(Value::Array(vec![Value::String("b".into())])),
            ),
        ];
        let (pid, result) = merge_server_results(results);
        assert_eq!(pid, PluginId(1));
        let arr = result.unwrap();
        assert_eq!(
            arr,
            Value::Array(
                vec![Value::String("a".into()), Value::String("b".into()),]
            )
        );
    }

    #[test]
    fn merge_filters_nulls() {
        let results = vec![
            (PluginId(1), Ok(Value::Null)),
            (PluginId(2), Ok(Value::String("hover info".into()))),
        ];
        let (pid, result) = merge_server_results(results);
        assert_eq!(pid, PluginId(2));
        assert_eq!(result.unwrap(), Value::String("hover info".into()));
    }

    #[test]
    fn merge_skips_errors_when_successes_exist() {
        let results = vec![
            (PluginId(1), Err(RpcError::new("server crashed"))),
            (
                PluginId(2),
                Ok(Value::Array(vec![Value::String("loc".into())])),
            ),
        ];
        let (pid, result) = merge_server_results(results);
        assert_eq!(pid, PluginId(2));
        assert!(result.is_ok());
    }

    #[test]
    fn merge_returns_error_when_all_fail() {
        let results = vec![
            (PluginId(1), Err(RpcError::new("error1"))),
            (PluginId(2), Err(RpcError::new("error2"))),
        ];
        let (_pid, result) = merge_server_results(results);
        assert!(result.is_err());
    }
}
