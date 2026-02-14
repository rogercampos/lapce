use std::{
    borrow::Cow,
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use lapce_rpc::{
    RpcError,
    plugin::{PluginId, VoltID, VoltInfo, VoltMetadata},
    proxy::ProxyResponse,
    style::LineStyle,
};
use lapce_xi_rope::{Rope, RopeDelta};
use lsp_types::{
    DidOpenTextDocumentParams, SemanticTokens, TextDocumentIdentifier,
    TextDocumentItem, VersionedTextDocumentIdentifier,
    notification::DidOpenTextDocument, request::Request,
};
use parking_lot::Mutex;
use psp_types::Notification;
use serde_json::Value;

use super::{
    PluginCatalogNotification, PluginCatalogRpcHandler,
    psp::{ClonableCallback, PluginServerRpc, PluginServerRpcHandler, RpcCallback},
    wasi::{load_all_volts, start_volt},
};
use crate::plugin::{
    install_volt, psp::PluginHandlerNotification, wasi::enable_volt,
};

/// Registry of all active and pending plugins. Runs on its own dedicated thread
/// (the "catalog thread") to serialize all plugin lifecycle operations.
///
/// - `plugins`: currently running plugin instances, keyed by their PluginId
/// - `unactivated_volts`: plugins that are installed but waiting for activation
///   triggers (e.g., opening a file of the right language, or a workspace_contains
///   glob matching)
/// - `open_files`: tracks which files are open and their language IDs, used to
///   determine whether unactivated plugins should be activated
/// - `plugin_configurations`: per-plugin settings from the user's config, forwarded
///   as initialization options when starting a plugin
pub struct PluginCatalog {
    workspace: Option<PathBuf>,
    plugin_rpc: PluginCatalogRpcHandler,
    plugins: HashMap<PluginId, PluginServerRpcHandler>,
    plugin_configurations: HashMap<String, HashMap<String, serde_json::Value>>,
    unactivated_volts: HashMap<VoltID, VoltMetadata>,
    open_files: HashMap<PathBuf, String>,
}

impl PluginCatalog {
    pub fn new(
        workspace: Option<PathBuf>,
        disabled_volts: Vec<VoltID>,
        extra_plugin_paths: Vec<PathBuf>,
        plugin_configurations: HashMap<String, HashMap<String, serde_json::Value>>,
        plugin_rpc: PluginCatalogRpcHandler,
    ) -> Self {
        let plugin = Self {
            workspace,
            plugin_rpc: plugin_rpc.clone(),
            plugin_configurations,
            plugins: HashMap::new(),
            unactivated_volts: HashMap::new(),
            open_files: HashMap::new(),
        };

        thread::spawn(move || {
            load_all_volts(plugin_rpc, &extra_plugin_paths, disabled_volts);
        });

        plugin
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_server_request(
        &mut self,
        plugin_id: Option<PluginId>,
        request_sent: Option<Arc<AtomicUsize>>,
        method: Cow<'static, str>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
        f: Box<dyn ClonableCallback<Value, RpcError>>,
    ) {
        if let Some(plugin_id) = plugin_id {
            if let Some(plugin) = self.plugins.get(&plugin_id) {
                plugin.server_request_async(
                    method,
                    params,
                    language_id,
                    path,
                    check,
                    move |result| {
                        f(plugin_id, result);
                    },
                );
            } else {
                f(plugin_id, Err(RpcError::new("plugin doesn't exist")));
            }
            return;
        }

        if let Some(request_sent) = request_sent {
            // if there are no plugins installed the callback of the client is not called
            // so check if plugins list is empty
            if self.plugins.is_empty() {
                // Add a request
                request_sent.fetch_add(1, Ordering::Relaxed);

                // make a direct callback with an "error"
                f(
                    lapce_rpc::plugin::PluginId(0),
                    Err(RpcError::new(
                        "no available plugin could make a callback, because the plugins list is empty",
                    )),
                );
                return;
            } else {
                request_sent.fetch_add(self.plugins.len(), Ordering::Relaxed);
            }
        }
        for (plugin_id, plugin) in self.plugins.iter() {
            let f = dyn_clone::clone_box(&*f);
            let plugin_id = *plugin_id;
            plugin.server_request_async(
                method.clone(),
                params.clone(),
                language_id.clone(),
                path.clone(),
                check,
                move |result| {
                    f(plugin_id, result);
                },
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_server_notification(
        &mut self,
        plugin_id: Option<PluginId>,
        method: impl Into<Cow<'static, str>>,
        params: Value,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
    ) {
        if let Some(plugin_id) = plugin_id {
            if let Some(plugin) = self.plugins.get(&plugin_id) {
                plugin.server_notification(method, params, language_id, path, check);
            }

            return;
        }

        // Otherwise send it to all plugins
        let method = method.into();
        for (_, plugin) in self.plugins.iter() {
            plugin.server_notification(
                method.clone(),
                params.clone(),
                language_id.clone(),
                path.clone(),
                check,
            );
        }
    }

    pub fn shutdown_volt(
        &mut self,
        volt: VoltInfo,
        f: Box<dyn ClonableCallback<Value, RpcError>>,
    ) {
        let id = volt.id();
        for (plugin_id, plugin) in self.plugins.iter() {
            if plugin.volt_id == id {
                let f = dyn_clone::clone_box(&*f);
                let plugin_id = *plugin_id;
                plugin.server_request_async(
                    lsp_types::request::Shutdown::METHOD,
                    Value::Null,
                    None,
                    None,
                    false,
                    move |result| {
                        f(plugin_id, result);
                    },
                );
                plugin.shutdown();
            }
        }
    }

    fn start_unactivated_volts(&mut self, to_be_activated: Vec<VoltID>) {
        for id in to_be_activated.iter() {
            let workspace = self.workspace.clone();
            if let Some(meta) = self.unactivated_volts.remove(id) {
                let configurations =
                    self.plugin_configurations.get(&meta.name).cloned();
                tracing::debug!("{:?} {:?}", id, configurations);
                let plugin_rpc = self.plugin_rpc.clone();
                thread::spawn(move || {
                    if let Err(err) =
                        start_volt(workspace, configurations, plugin_rpc, meta)
                    {
                        tracing::error!("{:?}", err);
                    }
                });
            }
        }
    }

    /// Scans all unactivated volts and activates any whose activation conditions
    /// are now met. Activation can be triggered by:
    /// 1. Language match: an open file's language_id matches the plugin's activation.language
    /// 2. Workspace contains: a file matching the plugin's activation.workspace_contains
    ///    glob exists in the workspace (uses walkdir for full traversal)
    ///
    /// Note: The workspace_contains check does a full directory walk, which can be
    /// expensive for large workspaces. This runs on the catalog thread so it blocks
    /// other plugin operations during the scan.
    fn check_unactivated_volts(&mut self) {
        let to_be_activated: Vec<VoltID> = self
            .unactivated_volts
            .iter()
            .filter_map(|(id, meta)| {
                let contains = meta
                    .activation
                    .as_ref()
                    .and_then(|a| a.language.as_ref())
                    .map(|l| {
                        self.open_files
                            .iter()
                            .any(|(_, language_id)| l.contains(language_id))
                    })
                    .unwrap_or(false);
                if contains {
                    return Some(id.clone());
                }

                if let Some(workspace) = self.workspace.as_ref() {
                    if let Some(globs) = meta
                        .activation
                        .as_ref()
                        .and_then(|a| a.workspace_contains.as_ref())
                    {
                        let mut builder = globset::GlobSetBuilder::new();
                        for glob in globs {
                            match globset::Glob::new(glob) {
                                Ok(glob) => {
                                    builder.add(glob);
                                }
                                Err(err) => {
                                    tracing::error!("{:?}", err);
                                }
                            }
                        }
                        match builder.build() {
                            Ok(matcher) => {
                                if !matcher.is_empty() {
                                    for entry in walkdir::WalkDir::new(workspace)
                                        .into_iter()
                                        .flatten()
                                    {
                                        if matcher.is_match(entry.path()) {
                                            return Some(id.clone());
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::error!("{:?}", err);
                            }
                        }
                    }
                }

                None
            })
            .collect();
        self.start_unactivated_volts(to_be_activated);
    }

    pub fn handle_did_open_text_document(&mut self, document: TextDocumentItem) {
        match document.uri.to_file_path() {
            Ok(path) => {
                self.open_files.insert(path, document.language_id.clone());
            }
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }

        let to_be_activated: Vec<VoltID> = self
            .unactivated_volts
            .iter()
            .filter_map(|(id, meta)| {
                let contains = meta
                    .activation
                    .as_ref()
                    .and_then(|a| a.language.as_ref())
                    .map(|l| l.contains(&document.language_id))?;
                if contains { Some(id.clone()) } else { None }
            })
            .collect();
        self.start_unactivated_volts(to_be_activated);

        let path = document.uri.to_file_path().ok();
        for (_, plugin) in self.plugins.iter() {
            plugin.server_notification(
                DidOpenTextDocument::METHOD,
                DidOpenTextDocumentParams {
                    text_document: document.clone(),
                },
                Some(document.language_id.clone()),
                path.clone(),
                true,
            );
        }
    }

    pub fn handle_did_save_text_document(
        &mut self,
        language_id: String,
        path: PathBuf,
        text_document: TextDocumentIdentifier,
        text: Rope,
    ) {
        for (_, plugin) in self.plugins.iter() {
            plugin.handle_rpc(PluginServerRpc::DidSaveTextDocument {
                language_id: language_id.clone(),
                path: path.clone(),
                text_document: text_document.clone(),
                text: text.clone(),
            });
        }
    }

    /// Broadcasts a text change to all active plugins. The `change` mutex is shared
    /// across all plugins so the delta-to-change conversion is computed at most once
    /// per sync kind (full and incremental), regardless of how many plugins are active.
    pub fn handle_did_change_text_document(
        &mut self,
        language_id: String,
        document: VersionedTextDocumentIdentifier,
        delta: RopeDelta,
        text: Rope,
        new_text: Rope,
    ) {
        let change = Arc::new(Mutex::new((None, None)));
        for (_, plugin) in self.plugins.iter() {
            plugin.handle_rpc(PluginServerRpc::DidChangeTextDocument {
                language_id: language_id.clone(),
                document: document.clone(),
                delta: delta.clone(),
                text: text.clone(),
                new_text: new_text.clone(),
                change: change.clone(),
            });
        }
    }

    pub fn format_semantic_tokens(
        &self,
        plugin_id: PluginId,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    ) {
        if let Some(plugin) = self.plugins.get(&plugin_id) {
            plugin.handle_rpc(PluginServerRpc::FormatSemanticTokens {
                tokens,
                text,
                f,
            });
        } else {
            f.call(Err(RpcError::new("plugin doesn't exist")));
        }
    }

    pub fn handle_notification(&mut self, notification: PluginCatalogNotification) {
        use PluginCatalogNotification::*;
        match notification {
            UnactivatedVolts(volts) => {
                tracing::debug!("UnactivatedVolts {:?}", volts);
                for volt in volts {
                    let id = volt.id();
                    self.unactivated_volts.insert(id, volt);
                }
                self.check_unactivated_volts();
            }
            UpdatePluginConfigs(configs) => {
                tracing::debug!("UpdatePluginConfigs {:?}", configs);
                self.plugin_configurations = configs;
            }
            PluginServerLoaded(plugin) => {
                // When a new plugin/LSP finishes initialization, replay all currently
                // open documents so the server knows about them. This is necessary
                // because files may have been opened before the server started (due
                // to lazy activation). The proxy is the source of truth for open files.
                match self.plugin_rpc.proxy_rpc.get_open_files_content() {
                    Ok(ProxyResponse::GetOpenFilesContentResponse { items }) => {
                        for item in items {
                            let language_id = Some(item.language_id.clone());
                            let path = item.uri.to_file_path().ok();
                            plugin.server_notification(
                                DidOpenTextDocument::METHOD,
                                DidOpenTextDocumentParams {
                                    text_document: item,
                                },
                                language_id,
                                path,
                                true,
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::error!("{:?}", err);
                    }
                }

                let plugin_id = plugin.plugin_id;
                let spawned_by = plugin.spawned_by;

                self.plugins.insert(plugin.plugin_id, plugin);

                if let Some(spawned_by) = spawned_by {
                    if let Some(plugin) = self.plugins.get(&spawned_by) {
                        plugin.handle_rpc(PluginServerRpc::Handler(
                            PluginHandlerNotification::SpawnedPluginLoaded {
                                plugin_id,
                            },
                        ));
                    }
                }
            }
            InstallVolt(volt) => {
                tracing::debug!("InstallVolt {:?}", volt);
                let workspace = self.workspace.clone();
                let configurations =
                    self.plugin_configurations.get(&volt.name).cloned();
                let catalog_rpc = self.plugin_rpc.clone();
                catalog_rpc.stop_volt(volt.clone());
                thread::spawn(move || {
                    if let Err(err) =
                        install_volt(catalog_rpc, workspace, configurations, volt)
                    {
                        tracing::error!("{:?}", err);
                    }
                });
            }
            ReloadVolt(volt) => {
                tracing::debug!("ReloadVolt {:?}", volt);
                let volt_id = volt.id();
                let ids: Vec<PluginId> = self.plugins.keys().cloned().collect();
                for id in ids {
                    if self.plugins.get(&id).unwrap().volt_id == volt_id {
                        let plugin = self.plugins.remove(&id).unwrap();
                        plugin.shutdown();
                    }
                }
                if let Err(err) = self.plugin_rpc.unactivated_volts(vec![volt]) {
                    tracing::error!("{:?}", err);
                }
            }
            StopVolt(volt) => {
                tracing::debug!("StopVolt {:?}", volt);
                let volt_id = volt.id();
                let ids: Vec<PluginId> = self.plugins.keys().cloned().collect();
                for id in ids {
                    if self.plugins.get(&id).unwrap().volt_id == volt_id {
                        let plugin = self.plugins.remove(&id).unwrap();
                        plugin.shutdown();
                    }
                }
            }
            EnableVolt(volt) => {
                tracing::debug!("EnableVolt {:?}", volt);
                let volt_id = volt.id();
                for (_, volt) in self.plugins.iter() {
                    if volt.volt_id == volt_id {
                        return;
                    }
                }
                let plugin_rpc = self.plugin_rpc.clone();
                thread::spawn(move || {
                    if let Err(err) = enable_volt(plugin_rpc, volt) {
                        tracing::error!("{:?}", err);
                    }
                });
            }
            Shutdown => {
                for (_, plugin) in self.plugins.iter() {
                    plugin.shutdown();
                }
            }
        }
    }
}
