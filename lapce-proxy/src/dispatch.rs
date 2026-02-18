use std::{
    collections::HashMap,
    fs, io,
    path::PathBuf,
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
    core::{CoreNotification, CoreRpcHandler, FileChanged, GitFileStatus},
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
}

impl ProxyHandler for Dispatcher {
    fn handle_notification(&mut self, rpc: ProxyNotification) {
        use ProxyNotification::*;
        match rpc {
            Initialize {
                workspace,
                window_id: _,
                tab_id: _,
            } => {
                self.workspace = workspace;
                self.file_watcher.notify(FileWatchNotifier::new(
                    self.core_rpc.clone(),
                    self.proxy_rpc.clone(),
                    self.workspace.clone(),
                ));
                if let Some(workspace) = self.workspace.as_ref() {
                    self.file_watcher
                        .watch(workspace, true, WORKSPACE_EVENT_TOKEN);
                }

                if let Some(workspace) = self.workspace.as_ref() {
                    let branch = read_git_branch(workspace);
                    self.core_rpc.git_head_changed(branch);

                    let statuses = read_git_file_statuses(workspace);
                    self.core_rpc.git_file_status_changed(statuses);
                }

                let env =
                    crate::shell_env::resolve_shell_env(self.workspace.as_deref());
                self.lsp_rpc.set_shell_env(env);

                let lsp_rpc = self.lsp_rpc.clone();
                let workspace = self.workspace.clone();
                thread::spawn(move || {
                    let mut manager = LspManager::new(workspace, lsp_rpc.clone());
                    lsp_rpc.mainloop(&mut manager);
                });
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
                                    path,
                                    FileChanged::Change(content),
                                );
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
                self.buffers.insert(path, buffer);
                self.respond_rpc(
                    id,
                    Ok(ProxyResponse::NewBufferResponse { content, read_only }),
                );
            }
            BufferHead { .. } => {
                self.respond_rpc(id, Err(RpcError::new("git support removed")));
            }
            GlobalSearch {
                pattern,
                case_sensitive,
                whole_word,
                is_regex,
            } => {
                static WORKER_ID: AtomicU64 = AtomicU64::new(0);
                let our_id = WORKER_ID.fetch_add(1, Ordering::SeqCst) + 1;

                let workspace = self.workspace.clone();
                let buffers = self
                    .buffers
                    .iter()
                    .map(|p| p.0)
                    .cloned()
                    .collect::<Vec<PathBuf>>();
                let proxy_rpc = self.proxy_rpc.clone();

                thread::spawn(move || {
                    proxy_rpc.handle_response(
                        id,
                        search_in_path(
                            our_id,
                            &WORKER_ID,
                            workspace
                                .iter()
                                .flat_map(|w| ignore::Walk::new(w).flatten())
                                .chain(
                                    buffers.iter().flat_map(|p| {
                                        ignore::Walk::new(p).flatten()
                                    }),
                                )
                                .map(|p| p.into_path()),
                            &pattern,
                            case_sensitive,
                            whole_word,
                            is_regex,
                        ),
                    );
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
                thread::spawn(move || {
                    let result = if let Some(workspace) = workspace {
                        let git_folder =
                            ignore::overrides::OverrideBuilder::new(&workspace)
                                .add("!.git/")
                                .map(|git_folder| git_folder.build());

                        let walker = match git_folder {
                            Ok(Ok(git_folder)) => {
                                ignore::WalkBuilder::new(&workspace)
                                    .hidden(false)
                                    .parents(false)
                                    .require_git(false)
                                    .overrides(git_folder)
                                    .build()
                            }
                            _ => ignore::WalkBuilder::new(&workspace)
                                .parents(false)
                                .require_git(false)
                                .build(),
                        };

                        let mut items = Vec::new();
                        for path in walker.flatten() {
                            if let Some(file_type) = path.file_type() {
                                if file_type.is_file() {
                                    items.push(path.into_path());
                                }
                            }
                        }
                        Ok(ProxyResponse::GetFilesResponse { items })
                    } else {
                        Ok(ProxyResponse::GetFilesResponse { items: Vec::new() })
                    };
                    proxy_rpc.handle_response(id, result);
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
                let proxy_rpc = self.proxy_rpc.clone();
                thread::spawn(move || {
                    let result = fs::read_dir(path)
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
                        .map_err(|e| RpcError::new(e.to_string()));
                    proxy_rpc.handle_response(id, result);
                });
            }
            Save {
                rev,
                path,
                create_parents,
            } => {
                let result = match self.buffers.get_mut(&path) {
                    Some(buffer) => buffer
                        .save(rev, create_parents)
                        .map(|_r| {
                            self.lsp_rpc
                                .did_save_text_document(&path, buffer.rope.clone());
                            ProxyResponse::SaveResponse {}
                        })
                        .map_err(|e| RpcError::new(e.to_string())),
                    None => {
                        Err(RpcError::new(format!("No buffer for path: {path:?}")))
                    }
                };
                self.respond_rpc(id, result);
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
        }
    }

    fn respond_rpc(&self, id: RequestId, result: Result<ProxyResponse, RpcError>) {
        self.proxy_rpc.handle_response(id, result);
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
    workspace_fs_change_handler: Arc<Mutex<Option<Sender<bool>>>>,
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
    ) -> Self {
        Self {
            core_rpc,
            proxy_rpc,
            workspace,
            workspace_fs_change_handler: Arc::new(Mutex::new(None)),
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
                    self.core_rpc.git_head_changed(branch);
                }
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
        // The bool tracks whether we need to reload the file explorer tree
        // (creates/deletes/renames). Git status is always recomputed since
        // any file modification can change it.
        let mut handler = self.workspace_fs_change_handler.lock();
        if let Some(sender) = handler.as_mut() {
            let _ = sender.send(explorer_change);
            return;
        }
        let (sender, receiver) = crossbeam_channel::unbounded();
        let _ = sender.send(explorer_change);

        let local_handler = self.workspace_fs_change_handler.clone();
        let core_rpc = self.core_rpc.clone();
        let workspace = self.workspace.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));

            {
                local_handler.lock().take();
            }

            let mut explorer_change = false;
            for e in receiver {
                if e {
                    explorer_change = true;
                    break;
                }
            }
            if explorer_change {
                core_rpc.workspace_file_change();
            }
            if let Some(workspace) = workspace.as_ref() {
                let statuses = read_git_file_statuses(workspace);
                core_rpc.git_file_status_changed(statuses);
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

fn read_git_file_statuses(
    workspace: &std::path::Path,
) -> HashMap<PathBuf, GitFileStatus> {
    let mut result = HashMap::new();
    let Ok(repo) = git2::Repository::open(workspace) else {
        return result;
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    opts.recurse_untracked_dirs(true);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        return result;
    };
    for entry in statuses.iter() {
        let status = entry.status();
        let git_status = if status
            .intersects(git2::Status::INDEX_RENAMED | git2::Status::WT_RENAMED)
        {
            GitFileStatus::Renamed
        } else if status.intersects(git2::Status::INDEX_NEW | git2::Status::WT_NEW) {
            if status.intersects(git2::Status::WT_NEW)
                && !status.intersects(git2::Status::INDEX_NEW)
            {
                GitFileStatus::Untracked
            } else {
                GitFileStatus::Added
            }
        } else if status
            .intersects(git2::Status::INDEX_DELETED | git2::Status::WT_DELETED)
        {
            GitFileStatus::Deleted
        } else if status.intersects(
            git2::Status::INDEX_MODIFIED
                | git2::Status::WT_MODIFIED
                | git2::Status::INDEX_TYPECHANGE
                | git2::Status::WT_TYPECHANGE,
        ) {
            GitFileStatus::Modified
        } else {
            continue;
        };
        if let Some(path_str) = entry.path() {
            result.insert(workspace.join(path_str), git_status);
        }
    }
    result
}

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
    let mut matcher = RegexMatcherBuilder::new();
    let matcher = matcher.case_insensitive(!case_sensitive).word(whole_word);
    let matcher = if is_regex {
        matcher.build(pattern)
    } else {
        matcher.build_literals(&[&regex::escape(pattern)])
    };
    let matcher = matcher.map_err(|_| RpcError::new("can't build matcher"))?;
    let mut searcher = SearcherBuilder::new().build();

    for path in paths {
        if current_id.load(Ordering::SeqCst) != id {
            return Err(RpcError::new("expired search job"));
        }

        if path.is_file() {
            let mut line_matches = Vec::new();
            if let Err(err) = searcher.search_path(
                &matcher,
                path.clone(),
                UTF8(|lnum, line| {
                    if current_id.load(Ordering::SeqCst) != id {
                        return Ok(false);
                    }

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
            ) {
                {
                    tracing::error!("{:?}", err);
                }
            }
            if !line_matches.is_empty() {
                matches.insert(path.clone(), line_matches);
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
