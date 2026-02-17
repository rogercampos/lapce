use std::{
    env,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use floem::{
    ViewId,
    action::{open_file, remove_overlay},
    ext_event::create_ext_action,
    file::FileDialogOptions,
    keyboard::Modifiers,
    kurbo::Size,
    peniko::kurbo::{Point, Rect, Vec2},
    prelude::SignalTrack,
    reactive::{
        Memo, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        WriteSignal, use_context,
    },
    text::{Attrs, AttrsList, FamilyOwned, LineHeightValue, TextLayout},
    views::editor::text::SystemClipboard,
};
use indexmap::IndexMap;
use itertools::Itertools;
use lapce_core::{
    command::FocusCommand,
    cursor::CursorAffinity,
    directory::Directory,
    meta,
    register::{Clipboard, Register},
};
use lapce_rpc::{
    RpcError,
    core::CoreNotification,
    file::{Naming, PathObject},
    plugin::PluginId,
    proxy::{ProxyResponse, ProxyRpcHandler},
};
use lsp_types::{
    CodeActionOrCommand, CodeLens, Diagnostic, ProgressParams, ProgressToken,
    ShowMessageParams,
};
use serde_json::Value;
use tracing::{Level, debug, error, event};

use crate::{
    about::AboutData,
    alert::{AlertBoxData, AlertButton},
    code_action::{CodeActionData, CodeActionStatus},
    command::{
        CommandExecuted, CommandKind, InternalCommand, LapceCommand,
        LapceWorkbenchCommand, WindowCommand,
    },
    completion::{CompletionData, CompletionStatus},
    config::{LapceConfig, layout::LapceLayout},
    db::LapceDb,
    doc::DocContent,
    editor::location::{EditorLocation, EditorPosition},
    file_explorer::data::FileExplorerData,
    global_search::GlobalSearchData,
    go_to_file::GoToFileData,
    go_to_line::GoToLineData,
    go_to_symbol::GoToSymbolData,
    hover::HoverData,
    id::WorkspaceId,
    inline_completion::InlineCompletionData,
    keypress::{EventRef, KeyPressData, KeyPressFocus, condition::Condition},
    listener::Listener,
    lsp::path_from_url,
    main_split::{MainSplitData, SplitData, SplitDirection, SplitMoveDirection},
    panel::{
        data::{PanelData, PanelSection, default_panel_order},
        kind::PanelKind,
        position::PanelContainerPosition,
    },
    proxy::{ProxyData, new_proxy},
    recent_files::RecentFilesData,
    rename::RenameData,
    replace_modal::ReplaceModalData,
    search_modal::SearchModalData,
    search_tabs::SearchTabsData,
    tracing::*,
    window::WindowCommonData,
    workspace::{LapceWorkspace, LapceWorkspaceType, WorkspaceInfo},
};

/// Application-level focus state that determines which component receives keyboard
/// events. The `key_down()` dispatcher in WorkspaceData uses this to route events
/// to the correct KeyPressFocus implementor. Modal popups (About, RecentFiles,
/// SearchModal) set their respective focus variant when opened and restore
/// Focus::Workbench when closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    Workbench,
    CodeAction,
    Rename,
    AboutPopup,
    RecentFiles,
    SearchModal,
    ReplaceModal,
    GoToFile,
    GoToLine,
    GoToSymbol,
    Panel(PanelKind),
}

#[derive(Clone)]
pub struct WorkProgress {
    pub token: ProgressToken,
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u32>,
}

/// Shared state accessible to all components within a workspace tab.
/// Passed as `Rc<CommonData>` to avoid deep cloning. Contains the proxy RPC handle,
/// reactive config signal, focus state, and command listeners that any component
/// can use to send commands without knowing about the full WorkspaceData hierarchy.
#[derive(Clone)]
pub struct CommonData {
    pub workspace: Arc<LapceWorkspace>,
    pub scope: Scope,
    pub focus: RwSignal<Focus>,
    pub keypress: RwSignal<KeyPressData>,
    pub completion: RwSignal<CompletionData>,
    pub inline_completion: RwSignal<InlineCompletionData>,
    pub hover: HoverData,
    pub register: RwSignal<Register>,
    pub workbench_size: RwSignal<Size>,
    pub window_origin: RwSignal<Point>,
    pub internal_command: Listener<InternalCommand>,
    pub lapce_command: Listener<LapceCommand>,
    pub workbench_command: Listener<LapceWorkbenchCommand>,
    pub proxy: ProxyRpcHandler,
    pub view_id: RwSignal<ViewId>,
    pub ui_line_height: Memo<f64>,
    pub config: ReadSignal<Arc<LapceConfig>>,
    // the current focused view which will receive keyboard events
    pub keyboard_focus: RwSignal<Option<ViewId>>,
    pub window_common: Rc<WindowCommonData>,
}

impl std::fmt::Debug for CommonData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommonData")
            .field("workspace", &self.workspace)
            .finish()
    }
}

/// The per-workspace-tab state container. Each OS window tab has its own WorkspaceData
/// with its own proxy process, editor splits, panels, and configuration.
/// This is the central orchestrator that wires together all subsystems and handles
/// command dispatch via the three command listeners (lapce, workbench, internal).
#[derive(Clone)]
pub struct WorkspaceData {
    pub scope: Scope,
    pub workspace_id: WorkspaceId,
    pub workspace: Arc<LapceWorkspace>,
    pub main_split: MainSplitData,
    pub file_explorer: FileExplorerData,
    pub panel: PanelData,
    pub code_action: RwSignal<CodeActionData>,
    pub code_lens: RwSignal<Option<ViewId>>,
    pub rename: RenameData,
    pub global_search: GlobalSearchData,
    pub search_tabs: SearchTabsData,
    pub search_modal_data: SearchModalData,
    pub replace_modal_data: ReplaceModalData,
    pub about_data: AboutData,
    pub go_to_file_data: GoToFileData,
    pub go_to_line_data: GoToLineData,
    pub go_to_symbol_data: GoToSymbolData,
    pub recent_files: RwSignal<Vec<PathBuf>>,
    pub recent_files_data: RecentFilesData,
    pub alert_data: AlertBoxData,
    pub layout_rect: RwSignal<Rect>,
    pub title_height: RwSignal<f64>,
    pub status_height: RwSignal<f64>,
    pub proxy: ProxyData,
    pub set_config: WriteSignal<Arc<LapceConfig>>,
    pub update_in_progress: RwSignal<bool>,
    pub progresses: RwSignal<IndexMap<ProgressToken, WorkProgress>>,
    pub messages: RwSignal<Vec<(String, ShowMessageParams)>>,
    pub common: Rc<CommonData>,
}

impl std::fmt::Debug for WorkspaceData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceData")
            .field("workspace_id", &self.workspace_id)
            .finish()
    }
}

impl KeyPressFocus for WorkspaceData {
    fn check_condition(&self, condition: Condition) -> bool {
        match condition {
            Condition::PanelFocus => {
                matches!(self.common.focus.get_untracked(), Focus::Panel(_))
            }
            _ => false,
        }
    }

    fn run_command(
        &self,
        command: &LapceCommand,
        _count: Option<usize>,
        _mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Workbench(cmd) => {
                self.run_workbench_command(cmd.clone(), None);
            }
            CommandKind::Focus(cmd) => {
                if self.common.focus.get_untracked() == Focus::Workbench {
                    match cmd {
                        FocusCommand::SplitClose => {
                            self.main_split.editor_tab_child_close_active();
                        }
                        FocusCommand::SplitVertical => {
                            self.main_split.split_active(SplitDirection::Vertical);
                        }
                        FocusCommand::SplitHorizontal => {
                            self.main_split.split_active(SplitDirection::Horizontal);
                        }
                        FocusCommand::SplitRight => {
                            self.main_split
                                .split_move_active(SplitMoveDirection::Right);
                        }
                        FocusCommand::SplitLeft => {
                            self.main_split
                                .split_move_active(SplitMoveDirection::Left);
                        }
                        FocusCommand::SplitUp => {
                            self.main_split
                                .split_move_active(SplitMoveDirection::Up);
                        }
                        FocusCommand::SplitDown => {
                            self.main_split
                                .split_move_active(SplitMoveDirection::Down);
                        }
                        FocusCommand::SplitExchange => {
                            self.main_split.split_exchange_active();
                        }
                        _ => {
                            return CommandExecuted::No;
                        }
                    }
                }
            }
            _ => {
                return CommandExecuted::No;
            }
        }

        CommandExecuted::Yes
    }

    fn receive_char(&self, _c: &str) {}
}

impl WorkspaceData {
    /// Master initialization: creates the entire workspace state graph.
    /// Order matters: proxy must be started before components that send RPC calls,
    /// and the split tree must be populated from persisted info before components
    /// that reference main_split are created.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cx: Scope,
        workspace: Arc<LapceWorkspace>,
        window_common: Rc<WindowCommonData>,
    ) -> Self {
        // Child scope ensures all signals are cleaned up when this workspace tab is closed.
        let cx = cx.create_child();
        let db: Arc<LapceDb> = use_context().unwrap();

        // Load persisted workspace layout. For workspaces without a folder path
        // (bare windows), we clear the split children to avoid restoring stale
        // file editors that would fail to load.
        let workspace_info = if workspace.path.is_some() {
            db.get_workspace_info(&workspace).ok()
        } else {
            let mut info = db.get_workspace_info(&workspace).ok();
            if let Some(info) = info.as_mut() {
                info.split.children.clear();
            }
            info
        };

        let config = LapceConfig::load(&workspace);
        let lapce_command = Listener::new_empty(cx);
        let workbench_command = Listener::new_empty(cx);
        let internal_command = Listener::new_empty(cx);
        let keypress = cx.create_rw_signal(KeyPressData::new(cx, &config));

        // Start the proxy backend (runs as a thread in-process for local workspaces).
        // The proxy handles LSP, plugins, file watching, and global search.
        let proxy = new_proxy(workspace.clone());
        // Split config into read and write signals so that components only get
        // read access (via common.config) while only WorkspaceData can update it.
        let (config, set_config) = cx.create_signal(Arc::new(config));

        let focus = cx.create_rw_signal(Focus::Workbench);
        let completion = cx.create_rw_signal(CompletionData::new(cx, config));
        let inline_completion = cx.create_rw_signal(InlineCompletionData::new(cx));
        let hover = HoverData::new(cx);

        let register = cx.create_rw_signal(Register::default());
        let view_id = cx.create_rw_signal(ViewId::new());

        let ui_line_height = cx.create_memo(move |_| {
            let config = config.get();
            let mut text_layout = TextLayout::new();

            let family: Vec<FamilyOwned> =
                FamilyOwned::parse_list(&config.ui.font_family).collect();
            let attrs = Attrs::new()
                .family(&family)
                .font_size(config.ui.font_size() as f32)
                .line_height(LineHeightValue::Normal(
                    LapceLayout::UI_LINE_HEIGHT as f32,
                ));
            let attrs_list = AttrsList::new(attrs);
            text_layout.set_text("W", attrs_list, None);
            text_layout.size().height
        });

        let common = Rc::new(CommonData {
            workspace: workspace.clone(),
            scope: cx,
            keypress,
            focus,
            completion,
            inline_completion,
            hover,
            register,
            internal_command,
            lapce_command,
            workbench_command,
            proxy: proxy.proxy_rpc.clone(),
            view_id,
            ui_line_height,
            workbench_size: cx.create_rw_signal(Size::ZERO),
            config,
            window_origin: cx.create_rw_signal(Point::ZERO),
            keyboard_focus: cx.create_rw_signal(None),
            window_common: window_common.clone(),
        });

        let main_split = MainSplitData::new(cx, common.clone());
        let code_action =
            cx.create_rw_signal(CodeActionData::new(cx, common.clone()));
        let file_explorer =
            FileExplorerData::new(cx, main_split.editors, common.clone());

        // Restore the split tree from persisted workspace info, or create an empty root.
        // `to_data()` recursively reconstructs the entire split tree, editor tabs,
        // and editors from the serialized SplitInfo/EditorTabInfo/EditorInfo hierarchy.
        if let Some(info) = workspace_info.as_ref() {
            let root_split = main_split.root_split;
            info.split.to_data(main_split.clone(), None, root_split);
        } else {
            let root_split = main_split.root_split;
            let root_split_data = {
                let cx = cx.create_child();
                let root_split_data = SplitData {
                    scope: cx,
                    parent_split: None,
                    split_id: root_split,
                    children: Vec::new(),
                    direction: SplitDirection::Horizontal,
                    window_origin: Point::ZERO,
                    layout_rect: Rect::ZERO,
                };
                cx.create_rw_signal(root_split_data)
            };
            main_split.splits.update(|splits| {
                splits.insert(root_split, root_split_data);
            });
        }

        let title_height = cx.create_rw_signal(0.0);
        let status_height = cx.create_rw_signal(0.0);
        let panel_available_size = cx.create_memo(move |_| {
            let title_height = title_height.get();
            let status_height = status_height.get();
            let window_size = window_common.size.get();
            Size::new(
                window_size.width,
                window_size.height - title_height - status_height,
            )
        });
        let panel = workspace_info
            .as_ref()
            .map(|i| PanelData {
                panels: cx.create_rw_signal(default_panel_order()),
                styles: cx.create_rw_signal(i.panel.styles.clone()),
                size: cx.create_rw_signal(i.panel.size.clone()),
                available_size: panel_available_size,
                sections: cx.create_rw_signal(
                    i.panel
                        .sections
                        .iter()
                        .map(|(key, value)| (*key, cx.create_rw_signal(*value)))
                        .collect(),
                ),
                common: common.clone(),
            })
            .unwrap_or_else(|| {
                PanelData::new(
                    cx,
                    panel_available_size,
                    im::HashMap::new(),
                    common.clone(),
                )
            });

        let rename = RenameData::new(cx, main_split.editors, common.clone());
        let global_search = GlobalSearchData::new(cx, main_split.clone());
        let search_tabs =
            SearchTabsData::new(cx, main_split.clone(), common.clone());
        let search_modal_data = SearchModalData::new(
            cx,
            main_split.clone(),
            global_search.clone(),
            search_tabs.clone(),
            common.clone(),
        );
        let replace_modal_data = ReplaceModalData::new(
            cx,
            main_split.clone(),
            global_search.clone(),
            common.clone(),
        );

        let about_data = AboutData::new(cx, common.focus);
        let go_to_file_data = GoToFileData::new(
            cx,
            workspace.clone(),
            main_split.clone(),
            common.clone(),
        );
        let go_to_line_data =
            GoToLineData::new(cx, main_split.clone(), common.clone());
        let go_to_symbol_data = GoToSymbolData::new(
            cx,
            workspace.clone(),
            main_split.clone(),
            common.clone(),
        );
        let recent_files = cx.create_rw_signal(Vec::<PathBuf>::new());
        let recent_files_data = RecentFilesData::new(
            cx,
            main_split.clone(),
            recent_files,
            common.clone(),
        );
        let alert_data = AlertBoxData::new(cx, common.clone());

        // Restore search tabs from persisted workspace info
        if let Some(info) = workspace_info.as_ref() {
            if !info.search_tabs.is_empty() {
                search_tabs.restore_from_info(
                    info.search_tabs.clone(),
                    info.active_search_tab,
                );
            }
        }

        let workspace_data = Self {
            scope: cx,
            workspace_id: WorkspaceId::next(),
            workspace,
            main_split,
            panel,
            file_explorer,
            code_action,
            code_lens: cx.create_rw_signal(None),
            rename,
            global_search,
            search_tabs,
            search_modal_data,
            replace_modal_data,
            about_data,
            go_to_file_data,
            go_to_line_data,
            go_to_symbol_data,
            recent_files,
            recent_files_data,
            alert_data,
            layout_rect: cx.create_rw_signal(Rect::ZERO),
            title_height,
            status_height,
            proxy,
            set_config,
            update_in_progress: cx.create_rw_signal(false),
            progresses: cx.create_rw_signal(IndexMap::new()),
            messages: cx.create_rw_signal(Vec::new()),
            common,
        };

        // Reset the cursor blink timer whenever focus or active editor changes,
        // so the cursor is always visible immediately after switching. Also auto-dismiss
        // the rename popup when focus moves away from it.
        {
            let focus = workspace_data.common.focus;
            let active_editor = workspace_data.main_split.active_editor;
            let rename_active = workspace_data.rename.active;
            let internal_command = workspace_data.common.internal_command;
            cx.create_effect(move |_| {
                let focus = focus.get();
                active_editor.track();
                internal_command.send(InternalCommand::ResetBlinkCursor);

                if focus != Focus::Rename && rename_active.get_untracked() {
                    rename_active.set(false);
                }
            });
        }

        {
            let workspace_data = workspace_data.clone();
            workspace_data.common.lapce_command.listen(move |cmd| {
                workspace_data.run_lapce_command(cmd);
            });
        }

        {
            let workspace_data = workspace_data.clone();
            workspace_data.common.workbench_command.listen(move |cmd| {
                workspace_data.run_workbench_command(cmd, None);
            });
        }

        {
            let workspace_data = workspace_data.clone();
            let internal_command = workspace_data.common.internal_command;
            internal_command.listen(move |cmd| {
                workspace_data.run_internal_command(cmd);
            });
        }

        // Bridge proxy notifications into the reactive system. The proxy's CoreRpcHandler
        // sends notifications through an mpsc channel, which create_signal_from_channel
        // converts to a signal. This effect processes each notification (diagnostics,
        // completion responses, file changes, progress updates, etc.) as it arrives.
        {
            let workspace_data = workspace_data.clone();
            let notification = workspace_data.proxy.notification;
            cx.create_effect(move |_| {
                notification.with(|rpc| {
                    if let Some(rpc) = rpc.as_ref() {
                        workspace_data.handle_core_notification(rpc);
                    }
                });
            });
        }

        workspace_data
    }

    pub fn reload_config(&self) {
        let config = LapceConfig::load(&self.workspace);
        self.common.keypress.update(|keypress| {
            keypress.update_keymaps(&config);
        });
        self.set_config.set(Arc::new(config));
    }

    pub fn track_recent_file(&self, path: PathBuf) {
        self.recent_files.update(|files| {
            files.retain(|p| p != &path);
            files.insert(0, path);
            files.truncate(100);
        });
    }

    pub fn run_lapce_command(&self, cmd: LapceCommand) {
        match cmd.kind {
            CommandKind::Workbench(command) => {
                self.run_workbench_command(command, cmd.data);
            }
            CommandKind::Scroll(_)
            | CommandKind::Focus(_)
            | CommandKind::Edit(_)
            | CommandKind::Move(_) => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    editor_data.run_command(&cmd, None, Modifiers::empty());
                }
            }
            CommandKind::MotionMode(_) => {}
            CommandKind::MultiSelection(_) => {}
        }
    }

    pub fn run_workbench_command(
        &self,
        cmd: LapceWorkbenchCommand,
        data: Option<Value>,
    ) {
        use LapceWorkbenchCommand::*;
        match cmd {
            // ==== Files / Folders ====
            OpenFolder => {
                let window_command = self.common.window_common.window_command;
                let mut options = FileDialogOptions::new().title("Choose a folder").select_directories();
                options = if let Some(parent) = self.workspace.path.as_ref().and_then(|x| x.parent()) {
                    options.force_starting_directory(parent)
                } else {
                    options
                };
                open_file(options, move |file| {
                    if let Some(mut file) = file {
                        let workspace = LapceWorkspace {
                            kind: LapceWorkspaceType::Local,
                            path: Some(if let Some(path) = file.path.pop() {
                                path
                            } else {
                                tracing::error!("No path");
                                return;
                            }),
                            last_open: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        };
                        window_command
                            .send(WindowCommand::SetWorkspace { workspace });
                    }
                });
            }
            NewFile => {
                self.main_split.new_file();
            }
            RevealActiveFileInFileExplorer => {
                if let Some(editor_data) = self.main_split.active_editor.get() {
                    let doc = editor_data.doc();
                    let path = if let DocContent::File { path, .. } =
                        doc.content.get_untracked()
                    {
                        Some(path)
                    } else {
                        None
                    };
                    let Some(path) = path else { return };
                    let path = path.parent().unwrap_or(&path);

                    open_uri(path);
                }
            }

            CopyFilePath => {
                if let Some(editor_data) = self.main_split.active_editor.get_untracked() {
                    if let DocContent::File { path, .. } = editor_data.doc().content.get_untracked() {
                        let mut clipboard = SystemClipboard::new();
                        clipboard.put_string(path.to_string_lossy());
                    }
                }
            }
            // ==== Configuration / Info Files and Folders ====
            OpenSettings => {
                self.main_split.open_settings();
            }
            OpenSettingsFile => {
                if let Some(path) = LapceConfig::settings_file() {
                    self.main_split.jump_to_location(
                        EditorLocation {
                            path,
                            position: None,
                            scroll_offset: None,

                            same_editor_tab: false,
                        },
                        None,
                    );
                }
            }
            OpenSettingsDirectory => {
                if let Some(dir) = Directory::config_directory() {
                    open_uri(&dir);
                }
            }
            OpenKeyboardShortcuts => {
                self.main_split.open_keymap();
            }
            OpenKeyboardShortcutsFile => {
                if let Some(path) = LapceConfig::keymaps_file() {
                    self.main_split.jump_to_location(
                        EditorLocation {
                            path,
                            position: None,
                            scroll_offset: None,

                            same_editor_tab: false,
                        },
                        None,
                    );
                }
            }
            OpenLogFile => {
                if let Some(dir) = Directory::logs_directory() {
                    self.open_paths(&[PathObject::from_path(
                        dir.join(format!(
                            "lapce.{}.log",
                            chrono::prelude::Local::now().format("%Y-%m-%d")
                        )),
                        false,
                    )])
                }
            }
            OpenLogsDirectory => {
                if let Some(dir) = Directory::logs_directory() {
                    open_uri(&dir);
                }
            }
            OpenProxyDirectory => {
                if let Some(dir) = Directory::proxy_directory() {
                    open_uri(&dir);
                }
            }
            OpenGrammarsDirectory => {
                if let Some(dir) = Directory::grammars_directory() {
                    open_uri(&dir);
                }
            }
            OpenQueriesDirectory => {
                if let Some(dir) = Directory::queries_directory() {
                    open_uri(&dir);
                }
            }

            ToggleInlayHints => {}

            // ==== Window ====
            ReloadWindow => {
                self.common.window_common.window_command.send(
                    WindowCommand::SetWorkspace {
                        workspace: (*self.workspace).clone(),
                    },
                );
            }
            NewWindow => {
                self.common
                    .window_common
                    .window_command
                    .send(WindowCommand::NewWindow);
            }
            // ==== Editor Tabs ====
            NextEditorTab => {
                if let Some(editor_tab_id) =
                    self.main_split.active_editor_tab.get_untracked()
                {
                    self.main_split.editor_tabs.with_untracked(|editor_tabs| {
                        let Some(editor_tab) = editor_tabs.get(&editor_tab_id)
                        else {
                            return;
                        };

                        let new_index = editor_tab.with_untracked(|editor_tab| {
                            if editor_tab.children.is_empty() {
                                None
                            } else if editor_tab.active
                                == editor_tab.children.len() - 1
                            {
                                Some(0)
                            } else {
                                Some(editor_tab.active + 1)
                            }
                        });

                        if let Some(new_index) = new_index {
                            editor_tab.update(|editor_tab| {
                                editor_tab.active = new_index;
                            });
                        }
                    });
                }
            }
            PreviousEditorTab => {
                if let Some(editor_tab_id) =
                    self.main_split.active_editor_tab.get_untracked()
                {
                    self.main_split.editor_tabs.with_untracked(|editor_tabs| {
                        let Some(editor_tab) = editor_tabs.get(&editor_tab_id)
                        else {
                            return;
                        };

                        let new_index = editor_tab.with_untracked(|editor_tab| {
                            if editor_tab.children.is_empty() {
                                None
                            } else if editor_tab.active == 0 {
                                Some(editor_tab.children.len() - 1)
                            } else {
                                Some(editor_tab.active - 1)
                            }
                        });

                        if let Some(new_index) = new_index {
                            editor_tab.update(|editor_tab| {
                                editor_tab.active = new_index;
                            });
                        }
                    });
                }
            }

            // ==== Navigation ====
            GoToLine => {
                self.go_to_line_data.open();
            }
            GoToFile => {
                self.go_to_file_data.open();
            }
            // ==== UI ====
            ZoomIn => {
                let mut scale =
                    self.common.window_common.window_scale.get_untracked();
                scale += 0.1;
                if scale > 4.0 {
                    scale = 4.0
                }
                self.common.window_common.window_scale.set(scale);

                LapceConfig::update_file(
                    "ui",
                    "scale",
                    toml_edit::Value::from(scale),
                );
            }
            ZoomOut => {
                let mut scale =
                    self.common.window_common.window_scale.get_untracked();
                scale -= 0.1;
                if scale < 0.1 {
                    scale = 0.1
                }
                self.common.window_common.window_scale.set(scale);

                LapceConfig::update_file(
                    "ui",
                    "scale",
                    toml_edit::Value::from(scale),
                );
            }
            ZoomReset => {
                self.common.window_common.window_scale.set(1.0);

                LapceConfig::update_file(
                    "ui",
                    "scale",
                    toml_edit::Value::from(1.0),
                );
            }

            ToggleMaximizedPanel => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.panel.toggle_maximize(&kind);
                    }
                } else {
                    self.panel.toggle_active_maximize();
                }
            }
            HidePanel => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.hide_panel(kind);
                    }
                }
            }
            ShowPanel => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.show_panel(kind);
                    }
                }
            }
            TogglePanelFocus => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.toggle_panel_focus(kind);
                    }
                }
            }
            TogglePanelVisual => {
                if let Some(data) = data {
                    if let Ok(kind) = serde_json::from_value::<PanelKind>(data) {
                        self.toggle_panel_visual(kind);
                    }
                }
            }
            TogglePanelLeftVisual => {
                self.toggle_container_visual(&PanelContainerPosition::Left);
            }
            TogglePanelRightVisual => {
                self.toggle_container_visual(&PanelContainerPosition::Right);
            }
            TogglePanelBottomVisual => {
                self.toggle_container_visual(&PanelContainerPosition::Bottom);
            }
            ToggleSearchFocus => {
                self.search_modal_data.open();
            }
            SearchModalOpenFullResults => {
                self.search_modal_data.open_full_results();
            }
            GlobalReplace => {
                self.replace_modal_data.open();
            }
            FocusEditor => {
                self.common.focus.set(Focus::Workbench);
            }
            OpenUIInspector => {
                self.common.view_id.get_untracked().inspect();
            }
            ShowEnvironment => {
                self.main_split.show_env();
            }

            GoToSymbol => {
                self.go_to_symbol_data.open();
            }
            // ==== UI ====
            RecentFiles => {
                self.recent_files_data.open();
            }
            ShowAbout => {
                self.about_data.open();
            }
            // ==== Updating ====
            RestartToUpdate => {
                if let Some(release) = self
                    .common
                    .window_common
                    .latest_release
                    .get_untracked()
                    .as_ref()
                {
                    let release = release.clone();
                    let update_in_progress = self.update_in_progress;
                    if release.version != *meta::VERSION {
                        if let Ok(process_path) = env::current_exe() {
                            update_in_progress.set(true);
                            let send = create_ext_action(
                                self.common.scope,
                                move |_started| {
                                    update_in_progress.set(false);
                                },
                            );
                            std::thread::Builder::new().name("RestartToUpdate".to_owned()).spawn(move || {
                                let do_update = || -> anyhow::Result<()> {
                                    let src =
                                        crate::update::download_release(&release)?;

                                    let path =
                                        crate::update::extract(&src, &process_path)?;

                                    crate::update::restart(&path)?;

                                    Ok(())
                                };

                                if let Err(err) = do_update() {
                                    error!("Failed to update: {err}");
                                }

                                send(false);
                            }).unwrap();
                        }
                    }
                }
            }

            // ==== Movement ====
            #[cfg(target_os = "macos")]
            InstallToPATH => {
                self.common.internal_command.send(
                    InternalCommand::ExecuteProcess {
                        program: String::from("osascript"),
                        arguments: vec![String::from("-e"), format!(r#"do shell script "ln -sf '{}' /usr/local/bin/lapce" with administrator privileges"#, std::env::args().next().unwrap())],
                    }
                )
            }
            #[cfg(target_os = "macos")]
            UninstallFromPATH => {
                self.common.internal_command.send(
                    InternalCommand::ExecuteProcess {
                        program: String::from("osascript"),
                        arguments: vec![String::from("-e"), String::from(r#"do shell script "rm /usr/local/bin/lapce" with administrator privileges"#)],
                    }
                )
            }
            JumpLocationForward => {
                self.main_split.jump_location_forward(false);
            }
            JumpLocationBackward => {
                self.main_split.jump_location_backward(false);
            }
            JumpLocationForwardLocal => {
                self.main_split.jump_location_forward(true);
            }
            JumpLocationBackwardLocal => {
                self.main_split.jump_location_backward(true);
            }
            Quit => {
                floem::quit_app();
            }
            RevealInPanel => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    self.show_panel(PanelKind::FileExplorer);
                    self.panel
                        .section_open(PanelSection::FileExplorer).update(|x| {
                        *x = true;
                    });
                    if let DocContent::File {path, ..} = editor_data.doc().content.get_untracked() {
                        self.file_explorer.reveal_in_file_tree(path);
                    }
                }
            }
            RevealInFileExplorer => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    if let DocContent::File {path, ..} = editor_data.doc().content.get_untracked() {
                        let path = path.parent().unwrap_or(&path);
                        if !path.exists() {
                            return;
                        }
                        if let Err(err) = open::that(path) {
                            error!(
                            "Failed to reveal file in system file explorer: {}",
                            err
                        );
                        }
                    }
                }
            }
            GoToLocation => {
                if let Some(editor_data) =
                    self.main_split.active_editor.get_untracked()
                {
                    let doc = editor_data.doc();
                    let path = match doc.loaded_file_path() {
                        Some(path) => path,
                        None => return,
                    };
                    let offset = editor_data.cursor().with_untracked(|c| c.offset());
                    let internal_command = self.common.internal_command;

                    internal_command.send(InternalCommand::GoToLocation { location: EditorLocation {
                        path,
                        position: Some(EditorPosition::Offset(offset)),
                        scroll_offset: None,
                        same_editor_tab: false,
                    } });
                }
            }
        }
    }

    pub fn run_internal_command(&self, cmd: InternalCommand) {
        match cmd {
            InternalCommand::ReloadConfig => {
                self.reload_config();
            }
            InternalCommand::UpdateLogLevel { level } => {
                // TODO: implement logging panel, runtime log level change
                debug!("{level}");
            }
            InternalCommand::OpenFile { path } => {
                self.main_split.jump_to_location(
                    EditorLocation {
                        path,
                        position: None,
                        scroll_offset: None,
                        same_editor_tab: false,
                    },
                    None,
                );
            }
            InternalCommand::OpenFileInNewTab { path } => {
                self.main_split.jump_to_location(
                    EditorLocation {
                        path,
                        position: None,
                        scroll_offset: None,
                        same_editor_tab: false,
                    },
                    None,
                );
            }
            InternalCommand::ReloadFileExplorer => {
                self.file_explorer.reload();
            }
            InternalCommand::TestPathCreation { new_path } => {
                let naming = self.file_explorer.naming;

                let send = create_ext_action(
                    self.scope,
                    move |response: Result<ProxyResponse, RpcError>| match response {
                        Ok(_) => {
                            naming.update(Naming::set_ok);
                        }
                        Err(err) => {
                            naming.update(|naming| naming.set_err(err.message));
                        }
                    },
                );

                self.common.proxy.test_create_at_path(new_path, send);
            }
            InternalCommand::FinishRenamePath {
                current_path,
                new_path,
            } => {
                let send_current_path = current_path.clone();
                let send_new_path = new_path.clone();
                let file_explorer = self.file_explorer.clone();
                let editors = self.main_split.editors;

                let send = create_ext_action(
                    self.scope,
                    move |response: Result<ProxyResponse, RpcError>| match response {
                        Ok(response) => {
                            // Get the canonicalized new path from the proxy.
                            let new_path =
                                if let ProxyResponse::CreatePathResponse { path } =
                                    response
                                {
                                    path
                                } else {
                                    send_new_path
                                };

                            // If the renamed item is a file, update any editors the file is open
                            // in to use the new path.
                            // If the renamed item is a directory, update any editors in which a
                            // file the renamed directory is an ancestor of is open to use the
                            // file's new path.
                            let renamed_editors_content: Vec<_> = editors
                                .with_editors_untracked(|editors| {
                                    editors
                                        .values()
                                        .map(|editor| editor.doc().content)
                                        .filter(|content| {
                                            content.with_untracked(|content| {
                                                match content {
                                                    DocContent::File {
                                                        path,
                                                        ..
                                                    } => path.starts_with(
                                                        &send_current_path,
                                                    ),
                                                    _ => false,
                                                }
                                            })
                                        })
                                        .collect()
                                });

                            for content in renamed_editors_content {
                                content.update(|content| {
                                    if let DocContent::File { path, .. } = content {
                                        if let Ok(suffix) =
                                            path.strip_prefix(&send_current_path)
                                        {
                                            *path = new_path.join(suffix);
                                        }
                                    }
                                });
                            }

                            file_explorer.reload();
                            file_explorer.naming.set(Naming::None);
                        }
                        Err(err) => {
                            file_explorer
                                .naming
                                .update(|naming| naming.set_err(err.message));
                        }
                    },
                );

                self.file_explorer.naming.update(Naming::set_pending);
                self.common
                    .proxy
                    .rename_path(current_path.clone(), new_path, send);
            }
            InternalCommand::FinishNewNode { is_dir, path } => {
                let file_explorer = self.file_explorer.clone();
                let internal_command = self.common.internal_command;

                let send = create_ext_action(
                    self.scope,
                    move |response: Result<ProxyResponse, RpcError>| {
                        match response {
                            Ok(response) => {
                                file_explorer.reload();
                                file_explorer.naming.set(Naming::None);

                                // Open a new file in the editor
                                if let ProxyResponse::CreatePathResponse { path } =
                                    response
                                {
                                    if !is_dir {
                                        internal_command.send(
                                            InternalCommand::OpenFile { path },
                                        );
                                    }
                                }
                            }
                            Err(err) => {
                                file_explorer
                                    .naming
                                    .update(|naming| naming.set_err(err.message));
                            }
                        }
                    },
                );

                self.file_explorer.naming.update(Naming::set_pending);
                if is_dir {
                    self.common.proxy.create_directory(path, send);
                } else {
                    self.common.proxy.create_file(path, send);
                }
            }
            InternalCommand::FinishDuplicate { source, path } => {
                let file_explorer = self.file_explorer.clone();

                let send = create_ext_action(
                    self.scope,
                    move |response: Result<_, RpcError>| {
                        if let Err(err) = response {
                            file_explorer
                                .naming
                                .update(|naming| naming.set_err(err.message));
                        } else {
                            file_explorer.reload();
                            file_explorer.naming.set(Naming::None);
                        }
                    },
                );

                self.file_explorer.naming.update(Naming::set_pending);
                self.common.proxy.duplicate_path(source, path, send);
            }
            InternalCommand::GoToLocation { location } => {
                self.main_split.go_to_location(location, None);
            }
            InternalCommand::JumpToLocation { location } => {
                self.main_split.jump_to_location(location, None);
            }
            InternalCommand::OpenSearchPanel => {
                self.panel.show_panel(&PanelKind::Search);
                self.common.focus.set(Focus::Panel(PanelKind::Search));
            }
            InternalCommand::Split {
                direction,
                editor_tab_id,
            } => {
                self.main_split.split(direction, editor_tab_id);
            }
            InternalCommand::SplitMove {
                direction,
                editor_tab_id,
            } => {
                self.main_split.split_move(direction, editor_tab_id);
            }
            InternalCommand::SplitExchange { editor_tab_id } => {
                self.main_split.split_exchange(editor_tab_id);
            }
            InternalCommand::EditorTabClose { editor_tab_id } => {
                self.main_split.editor_tab_close(editor_tab_id);
            }
            InternalCommand::EditorTabChildClose {
                editor_tab_id,
                child,
            } => {
                self.main_split
                    .editor_tab_child_close(editor_tab_id, child, false);
            }
            InternalCommand::EditorTabCloseByKind {
                editor_tab_id,
                child,
                kind,
            } => {
                self.main_split.editor_tab_child_close_by_kind(
                    editor_tab_id,
                    child,
                    kind,
                );
            }
            InternalCommand::ShowCodeActions {
                offset,
                mouse_click,
                plugin_id,
                code_actions,
            } => {
                let mut code_action = self.code_action.get_untracked();
                code_action.show(plugin_id, code_actions, offset, mouse_click);
                self.code_action.set(code_action);
            }
            InternalCommand::RunCodeAction { plugin_id, action } => {
                self.main_split.run_code_action(plugin_id, action);
            }
            InternalCommand::ApplyWorkspaceEdit { edit } => {
                self.main_split.apply_workspace_edit(&edit);
            }
            InternalCommand::SaveJumpLocation {
                path,
                offset,
                scroll_offset,
            } => {
                self.main_split
                    .save_jump_location(path, offset, scroll_offset);
            }
            InternalCommand::StartRename {
                path,
                placeholder,
                position,
                start,
            } => {
                self.rename.start(path, placeholder, start, position);
            }
            InternalCommand::FocusEditorTab { editor_tab_id } => {
                self.main_split.active_editor_tab.set(Some(editor_tab_id));
            }
            InternalCommand::OpenWebUri { uri } => {
                if !uri.is_empty() {
                    match open::that(&uri) {
                        Ok(_) => {
                            trace!(TraceLevel::TRACE, "opened web uri: {uri:?}");
                        }
                        Err(e) => {
                            trace!(
                                TraceLevel::ERROR,
                                "failed to open web uri: {uri:?}, error: {e}"
                            );
                        }
                    }
                }
            }
            InternalCommand::ShowAlert {
                title,
                msg,
                buttons,
            } => {
                self.show_alert(title, msg, buttons);
            }
            InternalCommand::HideAlert => {
                self.alert_data.active.set(false);
            }
            InternalCommand::SaveScratchDoc { doc } => {
                self.main_split.save_scratch_doc(doc);
            }
            InternalCommand::ResetBlinkCursor => {
                // All the editors share the blinking information and logic, so we can just reset
                // one of them.
                if let Some(e_data) = self.main_split.active_editor.get_untracked() {
                    e_data.editor.cursor_info.reset();
                }
            }
            InternalCommand::ExecuteProcess { program, arguments } => {
                let mut cmd = match std::process::Command::new(program)
                    .args(arguments)
                    .spawn()
                {
                    Ok(v) => v,
                    Err(e) => {
                        return event!(Level::ERROR, "Failed to spawn process: {e}");
                    }
                };

                match cmd.wait() {
                    Ok(v) => event!(Level::TRACE, "Process exited with status {v}"),
                    Err(e) => {
                        event!(Level::ERROR, "Proces exited with an error: {e}")
                    }
                };
            }
            InternalCommand::TrackRecentFile { path } => {
                self.track_recent_file(path);
            }
            InternalCommand::CloseSearchTab { index } => {
                self.search_tabs.close_tab(index);
            }
            InternalCommand::CloseAllSearchTabs => {
                self.search_tabs.close_all_tabs();
            }
        }
    }

    fn handle_core_notification(&self, rpc: &CoreNotification) {
        match rpc {
            CoreNotification::CompletionResponse {
                request_id,
                input,
                resp,
                plugin_id,
            } => {
                self.common.completion.update(|completion| {
                    completion.receive(*request_id, input, resp, *plugin_id);
                });

                let completion = self.common.completion.get_untracked();
                let editor_data = completion
                    .latest_editor_id
                    .and_then(|id| self.main_split.editors.editor_untracked(id));
                if let Some(editor_data) = editor_data {
                    let cursor_offset =
                        editor_data.cursor().with_untracked(|c| c.offset());
                    completion
                        .update_document_completion(&editor_data, cursor_offset);
                }
            }
            CoreNotification::PublishDiagnostics { diagnostics } => {
                let path = path_from_url(&diagnostics.uri);
                let diagnostics: im::Vector<Diagnostic> = diagnostics
                    .diagnostics
                    .clone()
                    .into_iter()
                    .sorted_by_key(|d| d.range.start)
                    .collect();

                self.main_split
                    .get_diagnostic_data(&path)
                    .diagnostics
                    .set(diagnostics);

                // inform the document about the diagnostics
                if let Some(doc) = self
                    .main_split
                    .docs
                    .with_untracked(|docs| docs.get(&path).cloned())
                {
                    doc.init_diagnostics();
                }
            }
            CoreNotification::ServerStatus { params } => {
                if params.is_ok() {
                    // todo filter by language
                    self.main_split.docs.with_untracked(|x| {
                        for doc in x.values() {
                            doc.get_code_lens();
                            doc.get_semantic_styles();
                            doc.get_folding_range();
                            doc.get_inlay_hints();
                            doc.get_pull_diagnostics();
                        }
                    });
                }
            }
            CoreNotification::OpenPaths { paths } => {
                self.open_paths(paths);
            }
            CoreNotification::OpenFileChanged { path, content } => {
                self.main_split.open_file_changed(path, content);
            }
            CoreNotification::WorkDoneProgress { progress } => {
                self.update_progress(progress);
            }
            CoreNotification::ShowMessage { title, message } => {
                self.show_message(title, message);
            }
            CoreNotification::Log {
                level,
                message,
                target,
            } => {
                use lapce_rpc::core::LogLevel;
                use tracing_log::log::{Level, log};

                let target = target.clone().unwrap_or(String::from("unknown"));

                match level {
                    LogLevel::Trace => {
                        log!(target: &target, Level::Trace, "{}", message);
                    }
                    LogLevel::Debug => {
                        log!(target: &target, Level::Debug, "{}", message);
                    }
                    LogLevel::Info => {
                        log!(target: &target, Level::Info, "{}", message);
                    }
                    LogLevel::Warn => {
                        log!(target: &target, Level::Warn, "{}", message);
                    }
                    LogLevel::Error => {
                        log!(target: &target, Level::Error, "{}", message);
                    }
                }
            }
            CoreNotification::LogMessage { message, target } => {
                use lsp_types::MessageType;
                use tracing_log::log::{Level, log};
                match message.typ {
                    MessageType::ERROR => {
                        log!(target: target, Level::Error, "{}", message.message)
                    }
                    MessageType::WARNING => {
                        log!(target: target, Level::Warn, "{}", message.message)
                    }
                    MessageType::INFO => {
                        log!(target: target, Level::Info, "{}", message.message)
                    }
                    MessageType::DEBUG => {
                        log!(target: target, Level::Debug, "{}", message.message)
                    }
                    MessageType::LOG => {
                        log!(target: target, Level::Debug, "{}", message.message)
                    }
                    _ => {}
                }
            }
            CoreNotification::WorkspaceFileChange => {
                self.file_explorer.reload();
            }
            _ => {}
        }
    }

    /// Master keyboard event dispatcher. Routes events based on the current Focus:
    /// 1. First dispatches to the focused component's KeyPressFocus impl
    /// 2. If unhandled, falls back to WorkspaceData's own KeyPressFocus impl
    ///    (which handles workbench-level commands like split/close)
    /// Alert dialog blocks all keyboard input when active.
    pub fn key_down<'a>(&self, event: impl Into<EventRef<'a>> + Copy) -> bool {
        if self.alert_data.active.get_untracked() {
            return false;
        }
        let focus = self.common.focus.get_untracked();
        let keypress = self.common.keypress.get_untracked();
        let handle = match focus {
            Focus::Workbench => self.main_split.key_down(event, &keypress),
            Focus::CodeAction => {
                let code_action = self.code_action.get_untracked();
                Some(keypress.key_down(event, &code_action))
            }
            Focus::Rename => Some(keypress.key_down(event, &self.rename)),
            Focus::AboutPopup => Some(keypress.key_down(event, &self.about_data)),
            Focus::RecentFiles => {
                Some(keypress.key_down(event, &self.recent_files_data))
            }
            Focus::SearchModal => {
                Some(keypress.key_down(event, &self.search_modal_data))
            }
            Focus::ReplaceModal => {
                Some(keypress.key_down(event, &self.replace_modal_data))
            }
            Focus::GoToFile => Some(keypress.key_down(event, &self.go_to_file_data)),
            Focus::GoToLine => Some(keypress.key_down(event, &self.go_to_line_data)),
            Focus::GoToSymbol => {
                Some(keypress.key_down(event, &self.go_to_symbol_data))
            }
            Focus::Panel(PanelKind::Search) => {
                if let Some(active_search) = self.search_tabs.active_search() {
                    Some(keypress.key_down(event, &active_search))
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(handle) = &handle {
            if handle.handled {
                true
            } else {
                keypress
                    .handle_keymatch(
                        self,
                        handle.keymatch.clone(),
                        handle.keypress.clone(),
                    )
                    .handled
            }
        } else {
            keypress.key_down(event, self).handled
        }
    }

    /// Serializes the current workspace state (split tree layout + panel configuration)
    /// for persistence. Called during save operations to capture the complete layout.
    pub fn workspace_info(&self) -> WorkspaceInfo {
        let main_split_data = self
            .main_split
            .splits
            .get_untracked()
            .get(&self.main_split.root_split)
            .cloned()
            .unwrap();
        WorkspaceInfo {
            split: main_split_data.get_untracked().split_info(self),
            panel: self.panel.panel_info(),
            search_tabs: self.search_tabs.tab_infos(),
            active_search_tab: self.search_tabs.active_tab.get_untracked(),
        }
    }

    pub fn hover_origin(&self) -> Option<Point> {
        if !self.common.hover.active.get_untracked() {
            return None;
        }

        let editor_id = self.common.hover.editor_id.get_untracked();
        let editor_data = self.main_split.editors.editor(editor_id)?;

        let (window_origin, viewport, editor) = (
            editor_data.window_origin(),
            editor_data.viewport(),
            &editor_data.editor,
        );

        // TODO(minor): affinity should be gotten from where the hover was started at.
        let (point_above, point_below) = editor.points_of_offset(
            self.common.hover.offset.get_untracked(),
            CursorAffinity::Forward,
        );

        let window_origin =
            window_origin.get() - self.common.window_origin.get().to_vec2();
        let viewport = viewport.get();
        let hover_size = self.common.hover.layout_rect.get().size();
        let tab_size = self.layout_rect.get().size();

        let mut origin = window_origin
            + Vec2::new(
                point_below.x - viewport.x0,
                (point_above.y - viewport.y0) - hover_size.height,
            );
        if origin.y < 0.0 {
            origin.y = window_origin.y + point_below.y - viewport.y0;
        }
        if origin.x + hover_size.width + 1.0 > tab_size.width {
            origin.x = tab_size.width - hover_size.width - 1.0;
        }
        if origin.x <= 0.0 {
            origin.x = 0.0;
        }

        Some(origin)
    }

    pub fn completion_origin(&self) -> Point {
        let completion = self.common.completion.get();
        if completion.status == CompletionStatus::Inactive {
            return Point::ZERO;
        }
        let config = self.common.config.get();
        let editor_data =
            if let Some(editor) = self.main_split.active_editor.get_untracked() {
                editor
            } else {
                return Point::ZERO;
            };

        let (window_origin, viewport, editor) = (
            editor_data.window_origin(),
            editor_data.viewport(),
            &editor_data.editor,
        );

        // TODO(minor): What affinity should we use for this? Probably just use the cursor's
        // original affinity..
        let (point_above, point_below) =
            editor.points_of_offset(completion.offset, CursorAffinity::Forward);

        let window_origin =
            window_origin.get() - self.common.window_origin.get().to_vec2();
        let viewport = viewport.get();
        let completion_size = completion.layout_rect.size();
        let tab_size = self.layout_rect.get().size();

        let mut origin = window_origin
            + Vec2::new(
                point_below.x
                    - viewport.x0
                    - config.editor.line_height() as f64
                    - 5.0,
                point_below.y - viewport.y0,
            );
        if origin.y + completion_size.height > tab_size.height {
            origin.y = window_origin.y + (point_above.y - viewport.y0)
                - completion_size.height;
        }
        if origin.x + completion_size.width + 1.0 > tab_size.width {
            origin.x = tab_size.width - completion_size.width - 1.0;
        }
        if origin.x <= 0.0 {
            origin.x = 0.0;
        }

        origin
    }

    pub fn code_action_origin(&self) -> Point {
        let code_action = self.code_action.get();
        let config = self.common.config.get();
        if code_action.status.get_untracked() == CodeActionStatus::Inactive {
            return Point::ZERO;
        }

        let tab_size = self.layout_rect.get().size();
        let code_action_size = code_action.layout_rect.size();

        let editor_data =
            if let Some(editor) = self.main_split.active_editor.get_untracked() {
                editor
            } else {
                return Point::ZERO;
            };

        let (window_origin, viewport, editor) = (
            editor_data.window_origin(),
            editor_data.viewport(),
            &editor_data.editor,
        );

        // TODO(minor): What affinity should we use for this?
        let (_point_above, point_below) =
            editor.points_of_offset(code_action.offset, CursorAffinity::Forward);

        let window_origin =
            window_origin.get() - self.common.window_origin.get().to_vec2();
        let viewport = viewport.get();

        let mut origin = window_origin
            + Vec2::new(
                if code_action.mouse_click {
                    0.0
                } else {
                    point_below.x - viewport.x0
                },
                point_below.y - viewport.y0,
            );

        if origin.y + code_action_size.height > tab_size.height {
            origin.y = origin.y
                - config.editor.line_height() as f64
                - code_action_size.height;
        }
        if origin.x + code_action_size.width + 1.0 > tab_size.width {
            origin.x = tab_size.width - code_action_size.width - 1.0;
        }
        if origin.x <= 0.0 {
            origin.x = 0.0;
        }

        origin
    }

    pub fn rename_origin(&self) -> Point {
        let config = self.common.config.get();
        if !self.rename.active.get() {
            return Point::ZERO;
        }

        let tab_size = self.layout_rect.get().size();
        let rename_size = self.rename.layout_rect.get().size();

        let editor_data =
            if let Some(editor) = self.main_split.active_editor.get_untracked() {
                editor
            } else {
                return Point::ZERO;
            };

        let (window_origin, viewport, editor) = (
            editor_data.window_origin(),
            editor_data.viewport(),
            &editor_data.editor,
        );

        // TODO(minor): What affinity should we use for this?
        let (_point_above, point_below) = editor.points_of_offset(
            self.rename.start.get_untracked(),
            CursorAffinity::Forward,
        );

        let window_origin =
            window_origin.get() - self.common.window_origin.get().to_vec2();
        let viewport = viewport.get();

        let mut origin = window_origin
            + Vec2::new(point_below.x - viewport.x0, point_below.y - viewport.y0);

        if origin.y + rename_size.height > tab_size.height {
            origin.y =
                origin.y - config.editor.line_height() as f64 - rename_size.height;
        }
        if origin.x + rename_size.width + 1.0 > tab_size.width {
            origin.x = tab_size.width - rename_size.width - 1.0;
        }
        if origin.x <= 0.0 {
            origin.x = 0.0;
        }

        origin
    }

    pub fn toggle_panel_visual(&self, kind: PanelKind) {
        if self.panel.is_panel_visible(&kind) {
            self.hide_panel(kind);
        } else {
            self.show_panel(kind);
        }
    }

    /// Toggle a specific kind of panel.
    fn toggle_panel_focus(&self, kind: PanelKind) {
        let should_hide = match kind {
            PanelKind::FileExplorer => {
                // Some panels don't accept focus (yet). Fall back to visibility check
                // in those cases.
                self.panel.is_panel_visible(&kind)
            }
            PanelKind::Search => self.is_panel_focused(kind),
        };
        if should_hide {
            self.hide_panel(kind);
        } else {
            self.show_panel(kind);
        }
    }

    /// Toggle a panel on one of the sides.
    fn toggle_container_visual(&self, position: &PanelContainerPosition) {
        let shown = !self.panel.is_container_shown(position, false);
        self.panel.set_shown(&position.first(), shown);
        self.panel.set_shown(&position.second(), shown);

        if shown {
            if let Some((kind, _)) = self
                .panel
                .active_panel_at_position(&position.second(), false)
            {
                self.show_panel(kind);
            }

            if let Some((kind, _)) = self
                .panel
                .active_panel_at_position(&position.first(), false)
            {
                self.show_panel(kind);
            }
        } else {
            if let Some((kind, _)) = self
                .panel
                .active_panel_at_position(&position.second(), false)
            {
                self.hide_panel(kind);
            }

            if let Some((kind, _)) = self
                .panel
                .active_panel_at_position(&position.first(), false)
            {
                self.hide_panel(kind);
            }
        }
    }

    fn is_panel_focused(&self, kind: PanelKind) -> bool {
        // Moving between e.g. Search and Problems doesn't affect focus, so we need to also check
        // visibility.
        self.common.focus.get_untracked() == Focus::Panel(kind)
            && self.panel.is_panel_visible(&kind)
    }

    fn hide_panel(&self, kind: PanelKind) {
        self.panel.hide_panel(&kind);
        self.common.focus.set(Focus::Workbench);
    }

    pub fn show_panel(&self, kind: PanelKind) {
        self.panel.show_panel(&kind);
        self.common.focus.set(Focus::Panel(kind));
    }

    pub fn open_paths(&self, paths: &[PathObject]) {
        let (folders, files): (Vec<&PathObject>, Vec<&PathObject>) =
            paths.iter().partition(|p| p.is_dir);

        for folder in folders {
            self.common.window_common.window_command.send(
                WindowCommand::SetWorkspace {
                    workspace: LapceWorkspace {
                        kind: self.workspace.kind.clone(),
                        path: Some(folder.path.clone()),
                        last_open: 0,
                    },
                },
            );
        }

        for file in files {
            let position = file.linecol.map(|pos| {
                EditorPosition::Position(lsp_types::Position {
                    line: pos.line.saturating_sub(1) as u32,
                    character: pos.column.saturating_sub(1) as u32,
                })
            });

            self.common
                .internal_command
                .send(InternalCommand::GoToLocation {
                    location: EditorLocation {
                        path: file.path.clone(),
                        position,
                        scroll_offset: None,
                        same_editor_tab: false,
                    },
                });
        }
    }

    pub fn show_alert(&self, title: String, msg: String, buttons: Vec<AlertButton>) {
        self.alert_data.title.set(title);
        self.alert_data.msg.set(msg);
        self.alert_data.buttons.set(buttons);
        self.alert_data.active.set(true);
    }

    fn update_progress(&self, progress: &ProgressParams) {
        let token = progress.token.clone();
        match &progress.value {
            lsp_types::ProgressParamsValue::WorkDone(progress) => match progress {
                lsp_types::WorkDoneProgress::Begin(progress) => {
                    let progress = WorkProgress {
                        token: token.clone(),
                        title: progress.title.clone(),
                        message: progress.message.clone(),
                        percentage: progress.percentage,
                    };
                    self.progresses.update(|p| {
                        p.insert(token, progress);
                    });
                }
                lsp_types::WorkDoneProgress::Report(report) => {
                    self.progresses.update(|p| {
                        if let Some(progress) = p.get_mut(&token) {
                            progress.message.clone_from(&report.message);
                            progress.percentage = report.percentage;
                        }
                    })
                }
                lsp_types::WorkDoneProgress::End(_) => {
                    self.progresses.update(|p| {
                        p.swap_remove(&token);
                    });
                }
            },
        }
    }

    fn show_message(&self, title: &str, message: &ShowMessageParams) {
        self.messages.update(|messages| {
            messages.push((title.to_string(), message.clone()));
        });
    }

    pub fn update_code_lens_id(&self, view_id: Option<ViewId>) {
        if let Some(Some(old_id)) = self.code_lens.try_update(|x| {
            let old = x.take();
            if let Some(id) = view_id {
                let _ = x.insert(id);
            }
            old
        }) {
            remove_overlay(old_id);
        }
    }

    pub fn show_code_lens(
        &self,
        mouse_click: bool,
        plugin_id: PluginId,
        offset: usize,
        lens: im::Vector<CodeLens>,
    ) {
        self.common
            .internal_command
            .send(InternalCommand::ShowCodeActions {
                offset,
                mouse_click,
                plugin_id,
                code_actions: lens
                    .into_iter()
                    .filter_map(|lens| {
                        Some(CodeActionOrCommand::Command(lens.command?))
                    })
                    .collect(),
            });
    }
}

/// Open path with the default application without blocking.
fn open_uri(path: &Path) {
    match open::that(path) {
        Ok(_) => {
            debug!("opened active file: {path:?}");
        }
        Err(e) => {
            error!("failed to open active file: {path:?}, error: {e}");
        }
    }
}
