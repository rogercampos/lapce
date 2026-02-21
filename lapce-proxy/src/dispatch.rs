use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use crossbeam_channel::Sender;
use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{SearcherBuilder, sinks::UTF8};
use indexmap::IndexMap;
use lapce_rpc::{
    RequestId, RpcError,
    buffer::BufferId,
    core::{
        CoreNotification, CoreRpcHandler, FileChanged, GitFileStatus, GitRepoState,
    },
    file::FileNodeItem,
    file_line::FileLine,
    proxy::{
        ProxyHandler, ProxyNotification, ProxyRequest, ProxyResponse,
        ProxyRpcHandler, SearchMatch,
    },
    style::{LineStyle, SemanticStyles},
};
use lapce_xi_rope::Rope;
use lsp_types::{
    CancelParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    NumberOrString, Position, Range, TextDocumentItem, Url,
    notification::{Cancel, Notification},
};
use parking_lot::Mutex;

use crate::{
    buffer::{Buffer, get_mod_time, load_file},
    lsp::{LspRpcHandler, manager::LspManager},
    semgrep::SemgrepRunner,
    watcher::{FileWatcher, Notify, WatchToken},
};

const OPEN_FILE_EVENT_TOKEN: WatchToken = WatchToken(1);
const WORKSPACE_EVENT_TOKEN: WatchToken = WatchToken(2);

pub struct Dispatcher {
    workspace: Option<PathBuf>,
    pub proxy_rpc: ProxyRpcHandler,
    core_rpc: CoreRpcHandler,
    lsp_rpc: LspRpcHandler,
    buffers: HashMap<PathBuf, Buffer>,
    file_watcher: FileWatcher,
    excluded_directories: Vec<String>,
    semgrep_initialized: bool,
    semgrep: Option<SemgrepRunner>,
}

impl ProxyHandler for Dispatcher {
    fn handle_notification(&mut self, rpc: ProxyNotification) {
        tracing::info!(
            "[dispatch] handle_notification: {:?}",
            std::mem::discriminant(&rpc)
        );
        use ProxyNotification::*;
        match rpc {
            Initialize {
                workspace,
                window_id: _,
                tab_id: _,
                ruby_lsp_exclude_gems,
                ruby_lsp_excluded_patterns,
                excluded_directories,
            } => {
                tracing::info!(
                    "[dispatch] Initialize start, workspace={:?}",
                    workspace
                );
                self.workspace = workspace;
                self.excluded_directories = excluded_directories;
                let git_status_cache =
                    Arc::new(Mutex::new(None::<HashMap<PathBuf, GitFileStatus>>));
                self.file_watcher.notify(FileWatchNotifier::new(
                    self.core_rpc.clone(),
                    self.proxy_rpc.clone(),
                    self.workspace.clone(),
                    git_status_cache.clone(),
                ));
                if let Some(workspace) = self.workspace.as_ref() {
                    self.file_watcher
                        .watch(workspace, true, WORKSPACE_EVENT_TOKEN);
                }

                // Send git branch/repo state quickly (just reads a few files)
                if let Some(workspace) = self.workspace.as_ref() {
                    tracing::info!("[dispatch] Reading git branch/repo state...");
                    let branch = read_git_branch(workspace);
                    let repo_state = read_git_repo_state(workspace);
                    self.core_rpc.git_head_changed(branch, repo_state);
                    tracing::info!("[dispatch] Git branch/repo state done");
                }

                tracing::info!(
                    "[dispatch] Spawning background thread for git status/projects/LSP"
                );
                // Move all heavy work (git file statuses, project detection,
                // shell env resolution, LSP startup) to a background thread
                // so we don't block the dispatcher from processing ReadDir
                // and other requests.
                let lsp_rpc = self.lsp_rpc.clone();
                let core_rpc = self.core_rpc.clone();
                let workspace = self.workspace.clone();

                // Pre-announce startup tasks as queued so the UI can show them
                // before work begins.
                let task_id_git = if workspace.is_some() {
                    let id = core_rpc.next_background_task_id();
                    core_rpc
                        .background_task_queued(id, "Scanning git status".into());
                    Some(id)
                } else {
                    None
                };
                let task_id_projects = if workspace.is_some() {
                    let id = core_rpc.next_background_task_id();
                    core_rpc.background_task_queued(id, "Detecting projects".into());
                    Some(id)
                } else {
                    None
                };
                let task_id_shell = {
                    let id = core_rpc.next_background_task_id();
                    core_rpc.background_task_queued(
                        id,
                        "Resolving shell environment".into(),
                    );
                    id
                };

                thread::spawn(move || {
                    tracing::info!("[bg-thread] Background thread started");

                    // Git file statuses can be very slow on large repos
                    // (scans all files including node_modules, etc.)
                    if let Some(ws) = workspace.as_ref() {
                        let task_id = task_id_git.unwrap();
                        core_rpc.background_task_started(
                            task_id,
                            "Scanning git status".into(),
                        );
                        tracing::info!("[bg-thread] Reading git file statuses...");
                        let statuses = read_git_file_statuses(ws);
                        tracing::info!(
                            "[bg-thread] Git file statuses done ({} entries)",
                            statuses.len()
                        );
                        core_rpc.git_file_status_changed(statuses.clone());
                        *git_status_cache.lock() = Some(statuses);
                        core_rpc.background_task_finished(task_id);
                    }
                    // Detect sub-projects
                    let mut projects = if let Some(ws) = workspace.as_ref() {
                        let task_id = task_id_projects.unwrap();
                        core_rpc.background_task_started(
                            task_id,
                            "Detecting projects".into(),
                        );
                        tracing::info!(
                            "[bg-thread] Detecting projects in {:?}...",
                            ws
                        );
                        let p = crate::project::detect_projects(ws);
                        tracing::info!("[bg-thread] Detected {} projects", p.len());
                        core_rpc.background_task_finished(task_id);
                        p
                    } else {
                        Vec::new()
                    };

                    // Populate lsp_servers from the static config table
                    // (doesn't need shell env)
                    for project in &mut projects {
                        project.lsp_servers = project
                            .languages
                            .first()
                            .map(|lang| {
                                crate::lsp::manager::lsp_servers_for_language(lang)
                                    .into_iter()
                                    .map(|s| s.to_string())
                                    .collect()
                            })
                            .unwrap_or_default();
                    }

                    // Send projects to UI. Tool versions and version manager
                    // will be enriched lazily when LSP servers activate.
                    core_rpc.projects_detected(projects.clone());

                    // Resolve default shell env (slow: spawns login shell)
                    core_rpc.background_task_started(
                        task_id_shell,
                        "Resolving shell environment".into(),
                    );
                    tracing::info!("[bg-thread] Resolving default shell env...");
                    let default_env =
                        crate::shell_env::resolve_shell_env(workspace.as_deref());
                    tracing::info!(
                        "[bg-thread] Default shell env resolved ({} vars)",
                        default_env.len()
                    );
                    core_rpc.background_task_finished(task_id_shell);
                    lsp_rpc.set_default_shell_env(default_env);

                    tracing::info!("[bg-thread] Starting LSP manager mainloop");
                    let mut manager = LspManager::new(
                        workspace,
                        lsp_rpc.clone(),
                        projects,
                        ruby_lsp_exclude_gems,
                        ruby_lsp_excluded_patterns,
                    );
                    lsp_rpc.mainloop(&mut manager);
                });
                tracing::info!(
                    "[dispatch] Initialize handler done, dispatcher is now free to process requests"
                );
            }
            OpenPaths { paths } => {
                self.core_rpc
                    .notification(CoreNotification::OpenPaths { paths });
            }
            OpenFileChanged { path } => {
                if path.exists() {
                    if let Some(buffer) = self.buffers.get(&path) {
                        if get_mod_time(&buffer.path) == buffer.mod_time {
                            return;
                        }
                        match load_file(&buffer.path) {
                            Ok(content) => {
                                self.core_rpc.open_file_changed(
                                    path.clone(),
                                    FileChanged::Change(content),
                                );
                                self.maybe_scan_semgrep(&path);
                            }
                            Err(err) => {
                                tracing::event!(
                                    tracing::Level::ERROR,
                                    "Failed to re-read file after change notification: {err}"
                                );
                            }
                        }
                    }
                } else {
                    self.buffers.remove(&path);
                    self.core_rpc.open_file_changed(path, FileChanged::Delete);
                }
            }
            Completion {
                request_id,
                path,
                input,
                position,
            } => {
                self.lsp_rpc.completion(request_id, &path, input, position);
            }
            SignatureHelp {
                request_id,
                path,
                position,
            } => {
                self.lsp_rpc.signature_help(request_id, &path, position);
            }
            UpdateExcludedDirectories {
                excluded_directories,
            } => {
                self.excluded_directories = excluded_directories;
            }
            Shutdown {} => {
                self.lsp_rpc.shutdown();
                self.proxy_rpc.shutdown();
            }
            Update { path, delta, rev } => {
                let Some(buffer) = self.buffers.get_mut(&path) else {
                    tracing::error!("Update for unknown buffer: {path:?}");
                    return;
                };
                let old_text = buffer.rope.clone();
                buffer.update(&delta, rev);
                self.lsp_rpc.did_change_text_document(
                    &path,
                    rev,
                    delta,
                    old_text,
                    buffer.rope.clone(),
                );
            }
            LspCancel { id } => {
                self.lsp_rpc.send_notification(
                    None,
                    Cancel::METHOD,
                    CancelParams {
                        id: NumberOrString::Number(id),
                    },
                    None,
                    None,
                );
            }
        }
    }

    fn handle_request(&mut self, id: RequestId, rpc: ProxyRequest) {
        tracing::info!(
            "[dispatch] handle_request id={}: {:?}",
            id,
            std::mem::discriminant(&rpc)
        );
        use ProxyRequest::*;
        match rpc {
            NewBuffer { buffer_id, path } => {
                let buffer = Buffer::new(buffer_id, path.clone());
                let content = buffer.rope.to_string();
                let read_only = buffer.read_only;
                self.lsp_rpc.did_open_document(
                    &path,
                    buffer.language_id.to_string(),
                    buffer.rev as i32,
                    content.clone(),
                );
                self.file_watcher.watch(&path, false, OPEN_FILE_EVENT_TOKEN);
                self.buffers.insert(path.clone(), buffer);
                self.respond_rpc(
                    id,
                    Ok(ProxyResponse::NewBufferResponse { content, read_only }),
                );
                self.maybe_scan_semgrep(&path);
            }
            BufferHead { .. } => {
                self.respond_rpc(id, Err(RpcError::new("git support removed")));
            }
            GlobalSearch {
                pattern,
                case_sensitive,
                whole_word,
                is_regex,
                max_results,
                search_id,
            } => {
                static WORKER_ID: AtomicU64 = AtomicU64::new(0);
                let our_id = WORKER_ID.fetch_add(1, Ordering::SeqCst) + 1;

                let workspace = self.workspace.clone();
                let proxy_rpc = self.proxy_rpc.clone();
                let core_rpc = self.core_rpc.clone();
                let excluded_directories = self.excluded_directories.clone();

                thread::spawn(move || {
                    let task_id = core_rpc.next_background_task_id();
                    core_rpc.background_task_started(
                        task_id,
                        "Searching workspace".into(),
                    );
                    let result = parallel_search(
                        our_id,
                        &WORKER_ID,
                        workspace.as_deref(),
                        &excluded_directories,
                        &pattern,
                        case_sensitive,
                        whole_word,
                        is_regex,
                        max_results,
                        search_id,
                        &core_rpc,
                    );
                    core_rpc.background_task_finished(task_id);
                    proxy_rpc.handle_response(id, result);
                });
            }
            GlobalReplace {
                pattern,
                replacement,
                case_sensitive,
                whole_word,
                is_regex,
            } => {
                let workspace = self.workspace.clone();
                let proxy_rpc = self.proxy_rpc.clone();
                let core_rpc = self.core_rpc.clone();
                let excluded_directories = self.excluded_directories.clone();

                thread::spawn(move || {
                    let task_id = core_rpc.next_background_task_id();
                    core_rpc.background_task_started(task_id, "Replace All".into());
                    let result = global_replace(
                        workspace.as_deref(),
                        &excluded_directories,
                        &pattern,
                        &replacement,
                        case_sensitive,
                        whole_word,
                        is_regex,
                        &core_rpc,
                        task_id,
                    );
                    core_rpc.background_task_finished(task_id);
                    proxy_rpc.handle_response(id, result);
                });
            }
            CompletionResolve {
                plugin_id,
                completion_item,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.completion_resolve(
                    plugin_id,
                    *completion_item,
                    move |result| {
                        let result = result.map(|item| {
                            ProxyResponse::CompletionResolveResponse {
                                item: Box::new(item),
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetHover {
                request_id,
                path,
                position,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.hover(&path, position, move |_, result| {
                    let result = result.map(|hover| ProxyResponse::HoverResponse {
                        request_id,
                        hover,
                    });
                    proxy_rpc.handle_response(id, result);
                });
            }
            GetSignature { .. } => {}
            GetReferences { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc
                    .get_references(&path, position, move |_, result| {
                        let result = result.map(|references| {
                            ProxyResponse::GetReferencesResponse { references }
                        });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            GetDefinition {
                request_id,
                path,
                position,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc
                    .get_definition(&path, position, move |_, result| {
                        let result = result.map(|definition| {
                            ProxyResponse::GetDefinitionResponse {
                                request_id,
                                definition,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            GetTypeDefinition {
                request_id,
                path,
                position,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.get_type_definition(
                    &path,
                    position,
                    move |_, result| {
                        let result = result.map(|definition| {
                            ProxyResponse::GetTypeDefinition {
                                request_id,
                                definition,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            ShowCallHierarchy { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.show_call_hierarchy(
                    &path,
                    position,
                    move |_, result| {
                        let result = result.map(|items| {
                            ProxyResponse::ShowCallHierarchyResponse { items }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            CallHierarchyIncoming {
                path,
                call_hierarchy_item,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.call_hierarchy_incoming(
                    &path,
                    call_hierarchy_item,
                    move |_, result| {
                        let result = result.map(|items| {
                            ProxyResponse::CallHierarchyIncomingResponse { items }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetInlayHints { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                let buffer = self.buffers.get(&path).unwrap();
                let range = Range {
                    start: Position::new(0, 0),
                    end: buffer.offset_to_position(buffer.len()),
                };
                self.lsp_rpc
                    .get_inlay_hints(&path, range, move |_, result| {
                        let result = result
                            .map(|hints| ProxyResponse::GetInlayHints { hints });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            GetDocumentDiagnostics { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc
                    .get_document_diagnostics(&path, move |_, result| {
                        let result = result.and_then(|report| match report {
                            DocumentDiagnosticReportResult::Report(
                                DocumentDiagnosticReport::Full(report),
                            ) => Ok(ProxyResponse::GetDocumentDiagnosticsResponse {
                                diagnostics: report
                                    .full_document_diagnostic_report
                                    .items,
                            }),
                            DocumentDiagnosticReportResult::Report(
                                DocumentDiagnosticReport::Unchanged(_),
                            ) => Err(RpcError::new("unchanged")),
                            DocumentDiagnosticReportResult::Partial(_) => {
                                Err(RpcError::new("partial not supported"))
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            GetInlineCompletions {
                path,
                position,
                trigger_kind,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.get_inline_completions(
                    &path,
                    position,
                    trigger_kind,
                    move |_, result| {
                        let result = result.map(|completions| {
                            ProxyResponse::GetInlineCompletions { completions }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetSemanticTokens { path } => {
                let buffer = self.buffers.get(&path).unwrap();
                let text = buffer.rope.clone();
                let rev = buffer.rev;
                let len = buffer.len();
                let local_path = path.clone();
                let proxy_rpc = self.proxy_rpc.clone();
                let lsp_rpc = self.lsp_rpc.clone();

                let handle_tokens =
                    move |result: Result<Vec<LineStyle>, RpcError>| match result {
                        Ok(styles) => {
                            proxy_rpc.handle_response(
                                id,
                                Ok(ProxyResponse::GetSemanticTokens {
                                    styles: SemanticStyles {
                                        rev,
                                        path: local_path,
                                        styles,
                                        len,
                                    },
                                }),
                            );
                        }
                        Err(e) => {
                            proxy_rpc.handle_response(id, Err(e));
                        }
                    };

                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc
                    .get_semantic_tokens(
                        &path,
                        move |plugin_id, result| match result {
                            Ok(result) => {
                                lsp_rpc.format_semantic_tokens(
                                    plugin_id,
                                    result,
                                    text,
                                    Box::new(handle_tokens),
                                );
                            }
                            Err(e) => {
                                proxy_rpc.handle_response(id, Err(e));
                            }
                        },
                    );
            }
            GetCodeActions {
                path,
                position,
                diagnostics,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.get_code_actions(
                    &path,
                    position,
                    diagnostics,
                    move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::GetCodeActionsResponse { plugin_id, resp }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetDocumentFormatting { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc
                    .get_document_formatting(&path, move |_, result| {
                        let result = result.map(|edits| {
                            ProxyResponse::GetDocumentFormatting { edits }
                        });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            PrepareRename { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc
                    .prepare_rename(&path, position, move |_, result| {
                        let result =
                            result.map(|resp| ProxyResponse::PrepareRename { resp });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            Rename {
                path,
                position,
                new_name,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc
                    .rename(&path, position, new_name, move |_, result| {
                        let result =
                            result.map(|edit| ProxyResponse::Rename { edit });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            GetFiles { .. } => {
                let workspace = self.workspace.clone();
                let proxy_rpc = self.proxy_rpc.clone();
                let core_rpc = self.core_rpc.clone();
                let excluded_directories = self.excluded_directories.clone();
                thread::spawn(move || {
                    let task_id = core_rpc.next_background_task_id();
                    core_rpc.background_task_started(
                        task_id,
                        "Indexing workspace files".into(),
                    );
                    if let Some(workspace) = workspace {
                        let overrides = Dispatcher::build_walk_overrides(
                            &workspace,
                            &excluded_directories,
                        );

                        let mut walk_builder = ignore::WalkBuilder::new(&workspace);
                        walk_builder.hidden(false).parents(false).require_git(false);
                        if let Some(overrides) = overrides {
                            walk_builder.overrides(overrides);
                        }

                        // Stream files as they're discovered via notifications.
                        let pending: Arc<Mutex<Vec<PathBuf>>> =
                            Arc::new(Mutex::new(Vec::new()));
                        let cancelled =
                            Arc::new(std::sync::atomic::AtomicBool::new(false));

                        // Background flusher: sends batches every 100ms.
                        let flush_pending = pending.clone();
                        let flush_core_rpc = core_rpc.clone();
                        let flush_cancelled = cancelled.clone();
                        let flusher = thread::spawn(move || {
                            while !flush_cancelled.load(Ordering::Relaxed) {
                                thread::sleep(Duration::from_millis(100));
                                let batch = {
                                    let mut guard = flush_pending.lock();
                                    if guard.is_empty() {
                                        continue;
                                    }
                                    std::mem::take(&mut *guard)
                                };
                                flush_core_rpc.get_files_diff(batch);
                            }
                            // Final flush
                            let batch = std::mem::take(&mut *flush_pending.lock());
                            if !batch.is_empty() {
                                flush_core_rpc.get_files_diff(batch);
                            }
                        });

                        walk_builder.build_parallel().run(|| {
                            let pending = pending.clone();
                            Box::new(move |entry| {
                                let entry = match entry {
                                    Ok(e) => e,
                                    Err(_) => return ignore::WalkState::Continue,
                                };
                                if entry.file_type().map_or(false, |ft| ft.is_file())
                                {
                                    pending.lock().push(entry.into_path());
                                }
                                ignore::WalkState::Continue
                            })
                        });

                        cancelled.store(true, Ordering::Relaxed);
                        let _ = flusher.join();
                        core_rpc.get_files_done();
                    } else {
                        core_rpc.get_files_done();
                    }
                    core_rpc.background_task_finished(task_id);
                    proxy_rpc.handle_response(
                        id,
                        Ok(ProxyResponse::GetFilesResponse { items: Vec::new() }),
                    );
                });
            }
            GetOpenFilesContent {} => {
                let items = self
                    .buffers
                    .iter()
                    .map(|(path, buffer)| TextDocumentItem {
                        uri: Url::from_file_path(path).unwrap(),
                        language_id: buffer.language_id.to_string(),
                        version: buffer.rev as i32,
                        text: buffer.get_document(),
                    })
                    .collect();
                let resp = ProxyResponse::GetOpenFilesContentResponse { items };
                self.proxy_rpc.handle_response(id, Ok(resp));
            }
            ReadDir { path } => {
                tracing::info!("[dispatch] ReadDir request for {:?}", path);
                let proxy_rpc = self.proxy_rpc.clone();
                thread::spawn(move || {
                    let result = fs::read_dir(&path)
                        .map(|entries| {
                            let mut items = entries
                                .into_iter()
                                .filter_map(|entry| {
                                    entry
                                        .map(|e| FileNodeItem {
                                            path: e.path(),
                                            is_dir: e.path().is_dir(),
                                            open: false,
                                            read: false,
                                            children: HashMap::new(),
                                            children_open_count: 0,
                                        })
                                        .ok()
                                })
                                .collect::<Vec<FileNodeItem>>();

                            items.sort();

                            ProxyResponse::ReadDirResponse { items }
                        })
                        .map_err(|e| {
                            tracing::error!(
                                "[dispatch] ReadDir error for {:?}: {}",
                                path,
                                e
                            );
                            RpcError::new(e.to_string())
                        });
                    match &result {
                        Ok(ProxyResponse::ReadDirResponse { items }) => {
                            tracing::info!(
                                "[dispatch] ReadDir response for {:?}: {} items",
                                path,
                                items.len()
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "[dispatch] ReadDir failed for {:?}: {:?}",
                                path,
                                e
                            );
                        }
                        _ => {}
                    }
                    proxy_rpc.handle_response(id, result);
                });
            }
            Save {
                rev,
                path,
                create_parents,
            } => {
                let mut save_ok = false;
                let result = match self.buffers.get_mut(&path) {
                    Some(buffer) => buffer
                        .save(rev, create_parents)
                        .map(|_r| {
                            self.lsp_rpc
                                .did_save_text_document(&path, buffer.rope.clone());
                            save_ok = true;
                            ProxyResponse::SaveResponse {}
                        })
                        .map_err(|e| RpcError::new(e.to_string())),
                    None => {
                        Err(RpcError::new(format!("No buffer for path: {path:?}")))
                    }
                };
                self.respond_rpc(id, result);
                if save_ok {
                    self.maybe_scan_semgrep(&path);
                }
            }
            SaveBufferAs {
                buffer_id,
                path,
                rev,
                content,
                create_parents,
            } => {
                let mut buffer = Buffer::new(buffer_id, path.clone());
                buffer.rope = Rope::from(content);
                buffer.rev = rev;
                let result = buffer
                    .save(rev, create_parents)
                    .map(|_| ProxyResponse::Success {})
                    .map_err(|e| RpcError::new(e.to_string()));
                self.buffers.insert(path, buffer);
                self.respond_rpc(id, result);
            }
            CreateFile { path } => {
                let result = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| {
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(path)
                    })
                    .map(|_| ProxyResponse::Success {})
                    .map_err(|e| RpcError::new(e.to_string()));
                self.respond_rpc(id, result);
            }
            CreateDirectory { path } => {
                let result = std::fs::create_dir_all(path)
                    .map(|_| ProxyResponse::Success {})
                    .map_err(|e| RpcError::new(e.to_string()));
                self.respond_rpc(id, result);
            }
            TrashPath { path } => {
                let result = trash::delete(path)
                    .map(|_| ProxyResponse::Success {})
                    .map_err(|e| RpcError::new(e.to_string()));
                self.respond_rpc(id, result);
            }
            DuplicatePath {
                existing_path,
                new_path,
            } => {
                let result = if new_path.exists() {
                    Err(RpcError::new(format!("{new_path:?} already exists")))
                } else {
                    if let Some(parent) = new_path.parent() {
                        if let Err(error) = std::fs::create_dir_all(parent) {
                            let result = Err(RpcError::new(error.to_string()));
                            self.respond_rpc(id, result);
                            return;
                        }
                    }
                    std::fs::copy(existing_path, new_path)
                        .map(|_| ProxyResponse::Success {})
                        .map_err(|e| RpcError::new(e.to_string()))
                };
                self.respond_rpc(id, result);
            }
            RenamePath { from, to } => {
                let result = if to.exists() {
                    Err(format!("{} already exists", to.display()))
                } else {
                    Ok(())
                };

                let result = result.and_then(|_| {
                    if let Some(parent) = to.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            if let io::ErrorKind::AlreadyExists = e.kind() {
                                format!(
                                    "{} has a parent that is not a directory",
                                    to.display()
                                )
                            } else {
                                e.to_string()
                            }
                        })
                    } else {
                        Ok(())
                    }
                });

                let result = result
                    .and_then(|_| fs::rename(&from, &to).map_err(|e| e.to_string()));

                let result = result
                    .map(|_| {
                        let to = to.canonicalize().unwrap_or(to);

                        let (is_dir, is_file) = to
                            .metadata()
                            .map(|metadata| (metadata.is_dir(), metadata.is_file()))
                            .unwrap_or((false, false));

                        if is_dir {
                            let child_buffers: Vec<_> = self
                                .buffers
                                .keys()
                                .filter_map(|path| {
                                    path.strip_prefix(&from).ok().map(|suffix| {
                                        (path.clone(), suffix.to_owned())
                                    })
                                })
                                .collect();

                            for (path, suffix) in child_buffers {
                                if let Some(mut buffer) = self.buffers.remove(&path)
                                {
                                    let new_path = to.join(suffix);
                                    buffer.path = new_path;

                                    self.buffers.insert(buffer.path.clone(), buffer);
                                }
                            }
                        } else if is_file {
                            let buffer = self.buffers.remove(&from);

                            if let Some(mut buffer) = buffer {
                                buffer.path.clone_from(&to);
                                self.buffers.insert(to.clone(), buffer);
                            }
                        }

                        ProxyResponse::CreatePathResponse { path: to }
                    })
                    .map_err(RpcError::new);

                self.respond_rpc(id, result);
            }
            TestCreateAtPath { path } => {
                let result = if path.exists() {
                    Err(format!("{} already exists", path.display()))
                } else {
                    Ok(path)
                };

                let result = result
                    .and_then(|path| {
                        let parent_is_dir = path
                            .ancestors()
                            .skip(1)
                            .find(|parent| parent.exists())
                            .is_none_or(|parent| parent.is_dir());

                        if parent_is_dir {
                            Ok(ProxyResponse::Success {})
                        } else {
                            Err(format!(
                                "{} has a parent that is not a directory",
                                path.display()
                            ))
                        }
                    })
                    .map_err(RpcError::new);

                self.respond_rpc(id, result);
            }
            GetSelectionRange { positions, path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.get_selection_range(
                    path.as_path(),
                    positions,
                    move |_, result| {
                        let result = result.map(|ranges| {
                            ProxyResponse::GetSelectionRange { ranges }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            CodeActionResolve {
                action_item,
                plugin_id,
            } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.action_resolve(
                    *action_item,
                    plugin_id,
                    move |result| {
                        let result = result.map(|item| {
                            ProxyResponse::CodeActionResolveResponse {
                                item: Box::new(item),
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetCodeLens { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.get_code_lens(&path, move |plugin_id, result| {
                    let result = result.map(|resp| {
                        ProxyResponse::GetCodeLensResponse { plugin_id, resp }
                    });
                    proxy_rpc.handle_response(id, result);
                });
            }
            LspFoldingRange { path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.get_lsp_folding_range(
                    &path,
                    move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::LspFoldingRangeResponse {
                                plugin_id,
                                resp,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetCodeLensResolve { code_lens, path } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.get_code_lens_resolve(
                    &path,
                    &code_lens,
                    move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::GetCodeLensResolveResponse {
                                plugin_id,
                                resp,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GotoImplementation { path, position } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc.go_to_implementation(
                    &path,
                    position,
                    move |plugin_id, result| {
                        let result = result.map(|resp| {
                            ProxyResponse::GotoImplementationResponse {
                                plugin_id,
                                resp,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    },
                );
            }
            GetWorkspaceSymbols { path, query } => {
                let proxy_rpc = self.proxy_rpc.clone();
                self.lsp_rpc
                    .workspace_symbol(&path, query, move |_, result| {
                        let result = result.map(|response| {
                            use lsp_types::WorkspaceSymbolResponse;
                            let entries = match response {
                                Some(WorkspaceSymbolResponse::Flat(symbols)) => {
                                    symbols
                                        .into_iter()
                                        .map(|si| {
                                            lapce_rpc::proxy::SymbolInformationEntry {
                                                name: si.name,
                                                kind: si.kind,
                                                location: si.location,
                                                container_name: si.container_name,
                                            }
                                        })
                                        .collect()
                                }
                                Some(WorkspaceSymbolResponse::Nested(symbols)) => {
                                    symbols
                                        .into_iter()
                                        .filter_map(|ws| {
                                            let location = match ws.location {
                                                lsp_types::OneOf::Left(loc) => loc,
                                                lsp_types::OneOf::Right(wl) => {
                                                    lsp_types::Location {
                                                        uri: wl.uri,
                                                        range: Default::default(),
                                                    }
                                                }
                                            };
                                            Some(
                                                lapce_rpc::proxy::SymbolInformationEntry {
                                                    name: ws.name,
                                                    kind: ws.kind,
                                                    location,
                                                    container_name: ws
                                                        .container_name,
                                                },
                                            )
                                        })
                                        .collect()
                                }
                                None => Vec::new(),
                            };
                            ProxyResponse::GetWorkspaceSymbolsResponse {
                                symbols: entries,
                            }
                        });
                        proxy_rpc.handle_response(id, result);
                    });
            }
            ReferencesResolve { items } => {
                let items: Vec<FileLine> = items
                    .into_iter()
                    .filter_map(|location| {
                        let Ok(path) = location.uri.to_file_path() else {
                            tracing::error!(
                                "get file path fail: {:?}",
                                location.uri
                            );
                            return None;
                        };
                        let buffer = self.get_buffer_or_insert(path.clone());
                        let line_num = location.range.start.line as usize;
                        let content = buffer.line_to_cow(line_num).to_string();
                        Some(FileLine {
                            path,
                            position: location.range.start,
                            content,
                        })
                    })
                    .collect();
                let resp = ProxyResponse::ReferencesResolveResponse { items };
                self.proxy_rpc.handle_response(id, Ok(resp));
            }
        }
    }
}

impl Dispatcher {
    pub fn new(core_rpc: CoreRpcHandler, proxy_rpc: ProxyRpcHandler) -> Self {
        let lsp_rpc = LspRpcHandler::new(core_rpc.clone(), proxy_rpc.clone());

        let file_watcher = FileWatcher::new();

        Self {
            workspace: None,
            proxy_rpc,
            core_rpc,
            lsp_rpc,
            buffers: HashMap::new(),
            file_watcher,
            excluded_directories: Vec::new(),
            semgrep_initialized: false,
            semgrep: None,
        }
    }

    fn respond_rpc(&self, id: RequestId, result: Result<ProxyResponse, RpcError>) {
        self.proxy_rpc.handle_response(id, result);
    }

    fn build_walk_overrides(
        workspace: &std::path::Path,
        excluded_directories: &[String],
    ) -> Option<ignore::overrides::Override> {
        let mut builder = ignore::overrides::OverrideBuilder::new(workspace);
        builder.add("!.git/").ok()?;
        for dir in excluded_directories {
            builder.add(&format!("!{dir}/")).ok()?;
        }
        builder.build().ok()
    }

    fn init_semgrep(&mut self) {
        self.semgrep_initialized = true;
        if let Some(workspace) = self.workspace.as_ref() {
            let env = self.lsp_rpc.shell_env_for_project(Some(workspace));
            self.semgrep =
                SemgrepRunner::new(workspace.clone(), self.core_rpc.clone(), env);
        }
    }

    fn maybe_scan_semgrep(&mut self, path: &Path) {
        if !self.semgrep_initialized {
            self.init_semgrep();
        }
        if let Some(ref semgrep) = self.semgrep {
            semgrep.scan_file(path.to_path_buf());
        }
    }

    fn get_buffer_or_insert(&mut self, path: PathBuf) -> &mut Buffer {
        self.buffers
            .entry(path.clone())
            .or_insert(Buffer::new(BufferId::next(), path))
    }
}

struct FileWatchNotifier {
    core_rpc: CoreRpcHandler,
    proxy_rpc: ProxyRpcHandler,
    workspace: Option<PathBuf>,
    workspace_fs_change_handler: Arc<Mutex<Option<Sender<(bool, Vec<PathBuf>)>>>>,
    /// Cached git file statuses. `None` = not yet initialized (need full scan).
    git_status_cache: Arc<Mutex<Option<HashMap<PathBuf, GitFileStatus>>>>,
}

impl Notify for FileWatchNotifier {
    fn notify(&self, events: Vec<(WatchToken, notify::Event)>) {
        self.handle_fs_events(events);
    }
}

impl FileWatchNotifier {
    fn new(
        core_rpc: CoreRpcHandler,
        proxy_rpc: ProxyRpcHandler,
        workspace: Option<PathBuf>,
        git_status_cache: Arc<Mutex<Option<HashMap<PathBuf, GitFileStatus>>>>,
    ) -> Self {
        Self {
            core_rpc,
            proxy_rpc,
            workspace,
            workspace_fs_change_handler: Arc::new(Mutex::new(None)),
            git_status_cache,
        }
    }

    fn handle_fs_events(&self, events: Vec<(WatchToken, notify::Event)>) {
        for (token, event) in events {
            match token {
                OPEN_FILE_EVENT_TOKEN => self.handle_open_file_fs_event(event),
                WORKSPACE_EVENT_TOKEN => self.handle_workspace_fs_event(event),
                _ => {}
            }
        }
    }

    fn handle_open_file_fs_event(&self, event: notify::Event) {
        if event.kind.is_modify() || event.kind.is_remove() {
            for path in event.paths {
                #[cfg(windows)]
                if let Some(path_str) = path.to_str() {
                    const PREFIX: &str = r"\\?\";
                    if let Some(path_str) = path_str.strip_prefix(PREFIX) {
                        let path = PathBuf::from(&path_str);
                        self.proxy_rpc.notification(
                            ProxyNotification::OpenFileChanged { path },
                        );
                        continue;
                    }
                }
                self.proxy_rpc
                    .notification(ProxyNotification::OpenFileChanged { path });
            }
        }
    }

    fn handle_workspace_fs_event(&self, event: notify::Event) {
        if event.kind.is_modify() {
            if let Some(workspace) = self.workspace.as_ref() {
                let git_head = workspace.join(".git/HEAD");
                if event.paths.iter().any(|p| p == &git_head) {
                    let branch = read_git_branch(workspace);
                    let repo_state = read_git_repo_state(workspace);
                    self.core_rpc.git_head_changed(branch, repo_state);
                }
            }
        }

        // Detect repo state sentinel file changes (rebase, merge, cherry-pick, revert).
        // These fire immediately (not debounced) since it's just checking file existence.
        if let Some(workspace) = self.workspace.as_ref() {
            let git_dir = workspace.join(".git");
            let sentinel_files = [
                git_dir.join("rebase-merge"),
                git_dir.join("rebase-apply"),
                git_dir.join("MERGE_HEAD"),
                git_dir.join("CHERRY_PICK_HEAD"),
                git_dir.join("REVERT_HEAD"),
            ];
            let is_sentinel = event
                .paths
                .iter()
                .any(|p| sentinel_files.iter().any(|s| p == s || p.starts_with(s)));
            if is_sentinel {
                let branch = read_git_branch(workspace);
                let repo_state = read_git_repo_state(workspace);
                self.core_rpc.git_head_changed(branch, repo_state);
            }
        }

        let explorer_change = match &event.kind {
            notify::EventKind::Create(_)
            | notify::EventKind::Remove(_)
            | notify::EventKind::Modify(notify::event::ModifyKind::Name(_)) => true,
            notify::EventKind::Modify(_) => false,
            _ => return,
        };

        // Debounce: accumulate events for 500ms, then fire once.
        // The tuple tracks (explorer_change, changed_paths) so we can do
        // incremental git status updates for non-git file changes.
        let mut handler = self.workspace_fs_change_handler.lock();
        if let Some(sender) = handler.as_mut() {
            let _ = sender.send((explorer_change, event.paths.clone()));
            return;
        }
        let (sender, receiver) = crossbeam_channel::unbounded();
        let _ = sender.send((explorer_change, event.paths.clone()));

        let local_handler = self.workspace_fs_change_handler.clone();
        let core_rpc = self.core_rpc.clone();
        let workspace = self.workspace.clone();
        let git_cache = self.git_status_cache.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));

            {
                local_handler.lock().take();
            }

            let mut explorer_change = false;
            let mut changed_paths = Vec::new();
            for (e, paths) in receiver {
                if e {
                    explorer_change = true;
                }
                changed_paths.extend(paths);
            }
            if explorer_change {
                core_rpc.workspace_file_change();
            }
            if let Some(workspace) = workspace.as_ref() {
                // Decide: full rescan or incremental?
                // Full rescan if .git/ internals or .gitignore changed, or
                // if the cache hasn't been initialized yet.
                let needs_full = {
                    let cache = git_cache.lock();
                    cache.is_none()
                } || changed_paths.iter().any(|p| {
                    let rel = p.strip_prefix(workspace).unwrap_or(p);
                    rel.starts_with(".git")
                        || rel.file_name().map_or(false, |f| f == ".gitignore")
                });

                let task_id = core_rpc.next_background_task_id();
                if needs_full {
                    core_rpc.background_task_started(
                        task_id,
                        "Updating git status".into(),
                    );
                    let statuses = read_git_file_statuses(workspace);
                    core_rpc.git_file_status_changed(statuses.clone());
                    *git_cache.lock() = Some(statuses);
                } else {
                    core_rpc.background_task_started(
                        task_id,
                        "Updating git status".into(),
                    );
                    let incremental =
                        read_git_file_statuses_for_paths(workspace, &changed_paths);
                    let mut cache = git_cache.lock();
                    let cache_map = cache.as_mut().unwrap();
                    // Remove old entries for changed paths — if a file was
                    // reverted to clean, the incremental check won't return
                    // it, so we must clear the stale entry.
                    for path in &changed_paths {
                        cache_map.remove(path);
                    }
                    for (path, status) in incremental {
                        cache_map.insert(path, status);
                    }
                    core_rpc.git_file_status_changed(cache_map.clone());
                }
                core_rpc.background_task_finished(task_id);
            }
        });
        *handler = Some(sender);
    }
}

fn read_git_branch(workspace: &std::path::Path) -> Option<String> {
    let head_path = workspace.join(".git/HEAD");
    let content = fs::read_to_string(head_path).ok()?;
    let content = content.trim();
    if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else if content.len() >= 7 && content.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(content[..7].to_string())
    } else {
        None
    }
}

fn read_git_repo_state(workspace: &std::path::Path) -> GitRepoState {
    let git_dir = workspace.join(".git");
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists()
    {
        GitRepoState::Rebasing
    } else if git_dir.join("MERGE_HEAD").exists() {
        GitRepoState::Merging
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        GitRepoState::CherryPicking
    } else if git_dir.join("REVERT_HEAD").exists() {
        GitRepoState::Reverting
    } else {
        GitRepoState::Normal
    }
}

/// Map a git2 status bitfield to our GitFileStatus enum.
fn map_git_status(status: git2::Status) -> Option<GitFileStatus> {
    if status.intersects(git2::Status::CONFLICTED) {
        Some(GitFileStatus::Conflicted)
    } else if status
        .intersects(git2::Status::INDEX_RENAMED | git2::Status::WT_RENAMED)
    {
        Some(GitFileStatus::Renamed)
    } else if status.intersects(git2::Status::INDEX_NEW | git2::Status::WT_NEW) {
        if status.intersects(git2::Status::WT_NEW)
            && !status.intersects(git2::Status::INDEX_NEW)
        {
            Some(GitFileStatus::Untracked)
        } else {
            Some(GitFileStatus::Added)
        }
    } else if status
        .intersects(git2::Status::INDEX_DELETED | git2::Status::WT_DELETED)
    {
        Some(GitFileStatus::Deleted)
    } else if status.intersects(
        git2::Status::INDEX_MODIFIED
            | git2::Status::WT_MODIFIED
            | git2::Status::INDEX_TYPECHANGE
            | git2::Status::WT_TYPECHANGE,
    ) {
        Some(GitFileStatus::Modified)
    } else if status.intersects(git2::Status::IGNORED) {
        Some(GitFileStatus::Ignored)
    } else {
        None
    }
}

/// Collect statuses from a git2::Statuses iterator into our map.
fn collect_git_statuses(
    workspace: &std::path::Path,
    statuses: &git2::Statuses<'_>,
) -> HashMap<PathBuf, GitFileStatus> {
    let mut result = HashMap::new();
    for entry in statuses.iter() {
        if let Some(git_status) = map_git_status(entry.status()) {
            if let Some(path_str) = entry.path() {
                result.insert(workspace.join(path_str), git_status);
            }
        }
    }
    result
}

/// Full git status scan of the entire workspace.
fn read_git_file_statuses(
    workspace: &std::path::Path,
) -> HashMap<PathBuf, GitFileStatus> {
    let Ok(repo) = git2::Repository::open(workspace) else {
        return HashMap::new();
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    opts.recurse_untracked_dirs(true);
    opts.include_ignored(true);
    // Don't recurse into ignored directories (node_modules, target, etc.)
    // — individual ignored files like .env are still detected.
    opts.recurse_ignored_dirs(false);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        return HashMap::new();
    };
    collect_git_statuses(workspace, &statuses)
}

/// Incremental git status check for specific paths only.
/// Much faster than a full scan — only stats the given files.
fn read_git_file_statuses_for_paths(
    workspace: &std::path::Path,
    paths: &[PathBuf],
) -> HashMap<PathBuf, GitFileStatus> {
    let Ok(repo) = git2::Repository::open(workspace) else {
        return HashMap::new();
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    opts.include_ignored(false);
    // Add pathspecs relative to workspace root
    for path in paths {
        if let Ok(relative) = path.strip_prefix(workspace) {
            opts.pathspec(relative);
        }
    }
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        return HashMap::new();
    };
    collect_git_statuses(workspace, &statuses)
}

/// Search a single file for matches, returning the list of matches found.
fn search_file(
    matcher: &grep_regex::RegexMatcher,
    path: &std::path::Path,
) -> Vec<SearchMatch> {
    let mut searcher = SearcherBuilder::new().build();
    let mut line_matches = Vec::new();
    let _ = searcher.search_path(
        matcher,
        path,
        UTF8(|lnum, line| {
            let mymatch = matcher.find(line.as_bytes())?.unwrap();
            let (line, start, end) = if line.len() > 200 {
                let left_keep = line[..mymatch.start()]
                    .chars()
                    .rev()
                    .take(100)
                    .map(|c| c.len_utf8())
                    .sum::<usize>();
                let right_keep = line[mymatch.end()..]
                    .chars()
                    .take(100)
                    .map(|c| c.len_utf8())
                    .sum::<usize>();
                let display_range =
                    mymatch.start() - left_keep..mymatch.end() + right_keep;
                (
                    line[display_range].to_string(),
                    left_keep,
                    left_keep + (mymatch.end() - mymatch.start()),
                )
            } else {
                (line.to_string(), mymatch.start(), mymatch.end())
            };
            line_matches.push(SearchMatch {
                line: lnum as usize,
                start,
                end,
                line_content: line,
            });
            Ok(true)
        }),
    );
    line_matches
}

/// Parallel global search: walks the workspace using multiple threads and
/// searches each file as it is discovered.
///
/// Results are always streamed to the UI via `CoreNotification::GlobalSearchDiffMatches`
/// (tagged with `search_id`) so the user sees matches immediately. A background
/// flusher thread sends batches every 150ms. When `max_results` is set, the
/// search aborts early once the cap is reached. The response is always empty
/// (all data arrives via notifications).
fn parallel_search(
    id: u64,
    current_id: &AtomicU64,
    workspace: Option<&std::path::Path>,
    excluded_directories: &[String],
    pattern: &str,
    case_sensitive: bool,
    whole_word: bool,
    is_regex: bool,
    max_results: Option<usize>,
    search_id: u64,
    core_rpc: &CoreRpcHandler,
) -> Result<ProxyResponse, RpcError> {
    let mut builder = RegexMatcherBuilder::new();
    let builder = builder.case_insensitive(!case_sensitive).word(whole_word);
    let matcher = if is_regex {
        builder.build(pattern)
    } else {
        builder.build_literals(&[&regex::escape(pattern)])
    };
    let matcher = matcher.map_err(|_| RpcError::new("can't build matcher"))?;

    let workspace = match workspace {
        Some(w) => w,
        None => {
            core_rpc.global_search_done(search_id);
            return Ok(ProxyResponse::GlobalSearchResponse {
                matches: IndexMap::new(),
            });
        }
    };

    let overrides =
        Dispatcher::build_walk_overrides(workspace, excluded_directories);
    let mut walk_builder = ignore::WalkBuilder::new(workspace);
    walk_builder.hidden(false).parents(false).require_git(false);
    if let Some(overrides) = overrides {
        walk_builder.overrides(overrides);
    }

    // Shared state for collecting results from parallel workers.
    let pending: Arc<Mutex<IndexMap<PathBuf, Vec<SearchMatch>>>> =
        Arc::new(Mutex::new(IndexMap::new()));
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let match_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Background flusher thread: periodically drains pending results and
    // sends them to the UI as streaming notifications.
    let flush_pending = pending.clone();
    let flush_core_rpc = core_rpc.clone();
    let flush_cancelled = cancelled.clone();
    let flusher = thread::spawn(move || {
        while !flush_cancelled.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(150));
            let batch = {
                let mut guard = flush_pending.lock();
                if guard.is_empty() {
                    continue;
                }
                std::mem::take(&mut *guard)
            };
            flush_core_rpc.global_search_diff_matches(search_id, batch);
        }
        // Final flush for any remaining results
        let batch = std::mem::take(&mut *flush_pending.lock());
        if !batch.is_empty() {
            flush_core_rpc.global_search_diff_matches(search_id, batch);
        }
    });

    // Parallel walk + search.
    walk_builder.build_parallel().run(|| {
        let matcher = matcher.clone();
        let pending = pending.clone();
        let match_count = match_count.clone();
        let current_id_val = id;

        Box::new(move |entry| {
            if current_id.load(Ordering::SeqCst) != current_id_val {
                return ignore::WalkState::Quit;
            }

            // Check if we've hit the result cap
            if let Some(limit) = max_results {
                if match_count.load(Ordering::Relaxed) >= limit {
                    return ignore::WalkState::Quit;
                }
            }

            let entry = match entry {
                Ok(e) => e,
                Err(_) => return ignore::WalkState::Continue,
            };

            if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.into_path();
            let line_matches = search_file(&matcher, &path);

            if !line_matches.is_empty() {
                match_count.fetch_add(line_matches.len(), Ordering::Relaxed);
                pending.lock().insert(path, line_matches);
            }

            ignore::WalkState::Continue
        })
    });

    // Stop the flusher and let it send any remaining results
    cancelled.store(true, Ordering::Relaxed);
    let _ = flusher.join();
    core_rpc.global_search_done(search_id);
    Ok(ProxyResponse::GlobalSearchResponse {
        matches: IndexMap::new(),
    })
}

/// Replace all occurrences of a pattern on a single line (proxy-side version).
fn replace_all_on_line(
    line: &str,
    pattern: &str,
    replacement: &str,
    case_sensitive: bool,
    whole_word: bool,
    is_regex: bool,
) -> String {
    if is_regex {
        let case_flag = if case_sensitive { "" } else { "(?i)" };
        let full_pattern = if whole_word {
            format!("{case_flag}\\b{pattern}\\b")
        } else {
            format!("{case_flag}{pattern}")
        };
        if let Ok(re) = regex::Regex::new(&full_pattern) {
            re.replace_all(line, replacement).to_string()
        } else {
            line.to_string()
        }
    } else {
        let mut result = String::new();
        let mut remaining = line;
        loop {
            let found = if case_sensitive {
                remaining.find(pattern)
            } else {
                let lower = remaining.to_lowercase();
                let lower_pat = pattern.to_lowercase();
                lower.find(&lower_pat)
            };
            let Some(pos) = found else {
                result.push_str(remaining);
                break;
            };
            let end = pos + pattern.len();

            if whole_word {
                let abs_start = line.len() - remaining.len() + pos;
                let abs_end = abs_start + pattern.len();
                let before_ok = abs_start == 0
                    || !line.as_bytes()[abs_start - 1].is_ascii_alphanumeric()
                        && line.as_bytes()[abs_start - 1] != b'_';
                let after_ok = abs_end >= line.len()
                    || !line.as_bytes()[abs_end].is_ascii_alphanumeric()
                        && line.as_bytes()[abs_end] != b'_';
                if before_ok && after_ok {
                    result.push_str(&remaining[..pos]);
                    result.push_str(replacement);
                    remaining = &remaining[end..];
                } else {
                    result.push_str(&remaining[..end]);
                    remaining = &remaining[end..];
                }
            } else {
                result.push_str(&remaining[..pos]);
                result.push_str(replacement);
                remaining = &remaining[end..];
            }
        }
        result
    }
}

/// Replace a single file's matching lines in-place. Returns true if the file was modified.
fn replace_in_file(
    matcher: &grep_regex::RegexMatcher,
    path: &std::path::Path,
    pattern: &str,
    replacement: &str,
    case_sensitive: bool,
    whole_word: bool,
    is_regex: bool,
) -> bool {
    let line_matches = search_file(matcher, path);
    if line_matches.is_empty() {
        return false;
    }

    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };

    let mut lines: Vec<String> =
        content.split('\n').map(|s| s.to_string()).collect();

    // Collect unique matched line numbers
    let mut matched_lines: Vec<usize> =
        line_matches.iter().map(|m| m.line).collect();
    matched_lines.dedup();

    for line_num in matched_lines {
        let line_idx = line_num.saturating_sub(1);
        if line_idx >= lines.len() {
            continue;
        }
        lines[line_idx] = replace_all_on_line(
            &lines[line_idx],
            pattern,
            replacement,
            case_sensitive,
            whole_word,
            is_regex,
        );
    }

    let new_content = lines.join("\n");
    if new_content == content {
        return false;
    }
    std::fs::write(path, &new_content).is_ok()
}

/// Background global replace: walks the workspace, replaces all matches in all files,
/// reports progress, and sends a GlobalReplaceDone notification with modified file paths.
fn global_replace(
    workspace: Option<&std::path::Path>,
    excluded_directories: &[String],
    pattern: &str,
    replacement: &str,
    case_sensitive: bool,
    whole_word: bool,
    is_regex: bool,
    core_rpc: &CoreRpcHandler,
    task_id: u64,
) -> Result<ProxyResponse, RpcError> {
    let mut builder = RegexMatcherBuilder::new();
    let builder = builder.case_insensitive(!case_sensitive).word(whole_word);
    let matcher = if is_regex {
        builder.build(pattern)
    } else {
        builder.build_literals(&[&regex::escape(pattern)])
    };
    let matcher = matcher.map_err(|_| RpcError::new("can't build matcher"))?;

    let workspace = match workspace {
        Some(w) => w,
        None => {
            core_rpc.global_replace_done(Vec::new());
            return Ok(ProxyResponse::GlobalReplaceResponse { modified_count: 0 });
        }
    };

    // Phase 1: Collect all file paths
    let overrides =
        Dispatcher::build_walk_overrides(workspace, excluded_directories);
    let mut walk_builder = ignore::WalkBuilder::new(workspace);
    walk_builder.hidden(false).parents(false).require_git(false);
    if let Some(overrides) = overrides {
        walk_builder.overrides(overrides);
    }

    let mut all_files: Vec<PathBuf> = Vec::new();
    for entry in walk_builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().map_or(false, |ft| ft.is_file()) {
            all_files.push(entry.into_path());
        }
    }

    let total = all_files.len();
    let mut modified_files: Vec<PathBuf> = Vec::new();
    let mut last_reported_pct: u32 = 0;

    // Phase 2: Process each file
    for (i, path) in all_files.iter().enumerate() {
        if replace_in_file(
            &matcher,
            path,
            pattern,
            replacement,
            case_sensitive,
            whole_word,
            is_regex,
        ) {
            modified_files.push(path.clone());
        }

        // Report progress every ~5%
        let pct = if total > 0 {
            ((i + 1) * 100 / total) as u32
        } else {
            100
        };
        if pct >= last_reported_pct + 5 || i + 1 == total {
            let msg = format!(
                "{}/{} files ({} replaced)",
                i + 1,
                total,
                modified_files.len()
            );
            core_rpc.background_task_progress(task_id, Some(msg), Some(pct));
            last_reported_pct = pct;
        }
    }

    let modified_count = modified_files.len();

    // Phase 3: Notify UI about modified files
    core_rpc.global_replace_done(modified_files);

    Ok(ProxyResponse::GlobalReplaceResponse { modified_count })
}

#[cfg(test)]
fn search_in_path(
    id: u64,
    current_id: &AtomicU64,
    paths: impl Iterator<Item = PathBuf>,
    pattern: &str,
    case_sensitive: bool,
    whole_word: bool,
    is_regex: bool,
) -> Result<ProxyResponse, RpcError> {
    let mut matches = IndexMap::new();
    let mut builder = RegexMatcherBuilder::new();
    let builder = builder.case_insensitive(!case_sensitive).word(whole_word);
    let matcher = if is_regex {
        builder.build(pattern)
    } else {
        builder.build_literals(&[&regex::escape(pattern)])
    };
    let matcher = matcher.map_err(|_| RpcError::new("can't build matcher"))?;

    for path in paths {
        if current_id.load(Ordering::SeqCst) != id {
            return Err(RpcError::new("expired search job"));
        }

        if path.is_file() {
            let line_matches = search_file(&matcher, &path);
            if !line_matches.is_empty() {
                matches.insert(path, line_matches);
            }
        }
    }

    Ok(ProxyResponse::GlobalSearchResponse { matches })
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::atomic::AtomicU64};

    use indexmap::IndexMap;
    use lapce_rpc::proxy::{ProxyResponse, SearchMatch};

    use super::search_in_path;

    fn setup_search_dir() -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();

        let file1 = dir.path().join("hello.txt");
        std::fs::write(&file1, "Hello World\nfoo bar\nHello Again\n").unwrap();

        let file2 = dir.path().join("test.rs");
        std::fs::write(&file2, "fn main() {\n    println!(\"hello\");\n}\n")
            .unwrap();

        let file3 = dir.path().join("empty.txt");
        std::fs::write(&file3, "").unwrap();

        (dir, vec![file1, file2, file3])
    }

    fn extract_matches(
        resp: Result<ProxyResponse, lapce_rpc::RpcError>,
    ) -> IndexMap<PathBuf, Vec<SearchMatch>> {
        match resp.unwrap() {
            ProxyResponse::GlobalSearchResponse { matches } => matches,
            other => panic!("unexpected response: {:?}", other),
        }
    }

    #[test]
    fn basic_case_sensitive_search() {
        let (_dir, files) = setup_search_dir();
        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            files.into_iter(),
            "Hello",
            true,
            false,
            false,
        ));

        assert_eq!(matches.len(), 1);
        let file_matches = matches.values().next().unwrap();
        assert_eq!(file_matches.len(), 2);
        assert_eq!(file_matches[0].line, 1);
        assert_eq!(file_matches[1].line, 3);
    }

    #[test]
    fn case_insensitive_search() {
        let (_dir, files) = setup_search_dir();
        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            files.into_iter(),
            "hello",
            false,
            false,
            false,
        ));

        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn whole_word_search() {
        let (_dir, files) = setup_search_dir();
        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            files.into_iter(),
            "foo",
            true,
            true,
            false,
        ));

        assert_eq!(matches.len(), 1);
        let file_matches = matches.values().next().unwrap();
        assert_eq!(file_matches.len(), 1);
        assert_eq!(file_matches[0].line, 2);
        assert!(file_matches[0].line_content.contains("foo"));
    }

    #[test]
    fn regex_search() {
        let (_dir, files) = setup_search_dir();
        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            files.into_iter(),
            r"fn\s+\w+",
            true,
            false,
            true,
        ));

        assert_eq!(matches.len(), 1);
        let file_matches = matches.values().next().unwrap();
        assert_eq!(file_matches.len(), 1);
        assert!(file_matches[0].line_content.contains("fn main"));
    }

    #[test]
    fn no_matches_returns_empty() {
        let (_dir, files) = setup_search_dir();
        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            files.into_iter(),
            "nonexistent_pattern_xyz",
            true,
            false,
            false,
        ));

        assert!(matches.is_empty());
    }

    #[test]
    fn invalid_regex_returns_error() {
        let (_dir, files) = setup_search_dir();
        let id = AtomicU64::new(1);

        let result = search_in_path(
            1,
            &id,
            files.into_iter(),
            "[invalid regex",
            true,
            false,
            true,
        );

        assert!(result.is_err());
    }

    #[test]
    fn cancelled_search_returns_error() {
        let (_dir, files) = setup_search_dir();
        let id = AtomicU64::new(999);

        let result =
            search_in_path(1, &id, files.into_iter(), "Hello", true, false, false);

        assert!(result.is_err());
    }

    #[test]
    fn skips_directories() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "match\n").unwrap();
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let id = AtomicU64::new(1);
        let paths = vec![file.clone(), subdir];

        let matches = extract_matches(search_in_path(
            1,
            &id,
            paths.into_iter(),
            "match",
            true,
            false,
            false,
        ));

        assert_eq!(matches.len(), 1);
        assert!(matches.contains_key(&file));
    }

    #[test]
    fn match_offsets_are_correct() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("offsets.txt");
        std::fs::write(&file, "abcHELLOxyz\n").unwrap();

        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            vec![file].into_iter(),
            "HELLO",
            true,
            false,
            false,
        ));

        let file_matches = matches.values().next().unwrap();
        assert_eq!(file_matches[0].start, 3);
        assert_eq!(file_matches[0].end, 8);
    }

    #[test]
    fn long_line_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("long.txt");
        let prefix = "a".repeat(150);
        let suffix = "b".repeat(150);
        let line = format!("{prefix}MATCH{suffix}\n");
        std::fs::write(&file, &line).unwrap();

        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            vec![file].into_iter(),
            "MATCH",
            true,
            false,
            false,
        ));

        let file_matches = matches.values().next().unwrap();
        assert!(file_matches[0].line_content.len() < line.len());
        assert!(file_matches[0].line_content.contains("MATCH"));
    }

    #[test]
    fn empty_file_no_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.txt");
        std::fs::write(&file, "").unwrap();

        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            vec![file].into_iter(),
            "anything",
            true,
            false,
            false,
        ));

        assert!(matches.is_empty());
    }

    #[test]
    fn empty_paths_iterator() {
        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            std::iter::empty(),
            "anything",
            true,
            false,
            false,
        ));

        assert!(matches.is_empty());
    }

    #[test]
    fn multiple_matches_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("multi.txt");
        std::fs::write(&file, "aaa\nbbb\naaa\nbbb\naaa\n").unwrap();

        let id = AtomicU64::new(1);

        let matches = extract_matches(search_in_path(
            1,
            &id,
            vec![file].into_iter(),
            "aaa",
            true,
            false,
            false,
        ));

        let file_matches = matches.values().next().unwrap();
        assert_eq!(file_matches.len(), 3);
        assert_eq!(file_matches[0].line, 1);
        assert_eq!(file_matches[1].line, 3);
        assert_eq!(file_matches[2].line, 5);
    }
}
