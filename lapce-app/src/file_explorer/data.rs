use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use floem::{
    action::show_context_menu,
    event::EventPropagation,
    ext_event::create_ext_action,
    keyboard::Modifiers,
    menu::{Menu, MenuItem},
    reactive::{ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    views::editor::text::SystemClipboard,
};
use globset::{Glob, GlobMatcher};
use lapce_core::{
    command::{EditCommand, FocusCommand},
    register::Clipboard,
};
use lapce_rpc::{
    file::{
        Duplicating, FileNodeItem, FileNodeViewKind, Naming, NamingState, NewNode,
        Renaming,
    },
    proxy::ProxyResponse,
};

use crate::{
    command::{CommandExecuted, CommandKind, InternalCommand, LapceCommand},
    config::LapceConfig,
    editor::EditorData,
    keypress::{KeyPressFocus, condition::Condition},
    main_split::Editors,
    workspace_data::CommonData,
};

enum RenamedPath {
    NotRenaming,
    NameUnchanged,
    Renamed {
        current_path: PathBuf,
        new_path: PathBuf,
    },
}

/// Core data model for the file explorer tree. The `root` signal holds the entire
/// file tree as a recursive FileNodeItem; mutations update the signal which triggers
/// the virtual_stack to re-render. `naming` tracks the current rename/create/duplicate
/// operation in progress, if any.
#[derive(Clone, Debug)]
pub struct FileExplorerData {
    /// The root of the file tree. Contains the workspace directory and all loaded
    /// children recursively. Updated via `root.update()` for in-place mutations.
    pub root: RwSignal<FileNodeItem>,
    /// Tracks whether the user is currently renaming, creating, or duplicating a node.
    /// When set, the file explorer shows an inline text input at the appropriate position.
    pub naming: RwSignal<Naming>,
    /// A local editor used for the inline naming text input. Shared across all naming
    /// operations (only one can be active at a time).
    pub naming_editor_data: EditorData,
    pub common: Rc<CommonData>,
    /// Set to scroll to a specific line (e.g., after reveal_in_file_tree).
    pub scroll_to_line: RwSignal<Option<f64>>,
    /// The currently selected/highlighted item in the tree.
    pub select: RwSignal<Option<FileNodeViewKind>>,
    /// Set of starred (pinned) first-level folder paths, displayed at the top of the explorer.
    /// Paths are stored as full absolute paths.
    pub starred: RwSignal<HashSet<PathBuf>>,
    /// Compiled `files_exclude` glob, cached so every `ReadDir` response reuses
    /// the same matcher instead of reparsing and re-compiling the pattern.
    /// Recomputes (via a `create_effect` on the pattern string) only when the
    /// config's `files_exclude` pattern actually changes.
    pub files_exclude: RwSignal<Option<Rc<GlobMatcher>>>,
}

impl KeyPressFocus for FileExplorerData {
    /// The file explorer only handles keyboard input when a naming operation is active
    /// (renaming, creating, or duplicating). In that state, ModalFocus is reported so
    /// ESC can cancel the operation. When not naming, returns false for all conditions,
    /// effectively deferring to the default panel keyboard handling.
    fn check_condition(&self, condition: Condition) -> bool {
        self.naming.with_untracked(Naming::is_accepting_input)
            && condition == Condition::ModalFocus
    }

    fn run_command(
        &self,
        command: &LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        if self.naming.with_untracked(Naming::is_accepting_input) {
            match command.kind {
                CommandKind::Focus(FocusCommand::ModalClose) => {
                    self.cancel_naming();
                    CommandExecuted::Yes
                }
                CommandKind::Edit(EditCommand::InsertNewLine) => {
                    self.finish_naming();
                    CommandExecuted::Yes
                }
                CommandKind::Edit(_) => {
                    let command_executed =
                        self.naming_editor_data.run_command(command, count, mods);

                    if let Some(new_path) = self.naming_path() {
                        self.common
                            .internal_command
                            .send(InternalCommand::TestPathCreation { new_path });
                    }

                    command_executed
                }
                _ => self.naming_editor_data.run_command(command, count, mods),
            }
        } else {
            CommandExecuted::No
        }
    }

    fn receive_char(&self, c: &str) {
        if self.naming.with_untracked(Naming::is_accepting_input) {
            self.naming_editor_data.receive_char(c);

            if let Some(new_path) = self.naming_path() {
                self.common
                    .internal_command
                    .send(InternalCommand::TestPathCreation { new_path });
            }
        }
    }
}

impl FileExplorerData {
    pub fn new(
        cx: Scope,
        editors: Editors,
        common: Rc<CommonData>,
        initial_starred: Vec<PathBuf>,
    ) -> Self {
        let path = common.workspace.path.clone().unwrap_or_default();
        let root = cx.create_rw_signal(FileNodeItem {
            path: path.clone(),
            is_dir: true,
            read: false,
            open: false,
            children: HashMap::new(),
            children_open_count: 0,
            sorted: Vec::new(),
        });
        let naming = cx.create_rw_signal(Naming::None);
        let naming_editor_data = editors.make_local(cx, common.clone());
        let starred =
            cx.create_rw_signal(initial_starred.into_iter().collect::<HashSet<_>>());
        let files_exclude = cx.create_rw_signal(None::<Rc<GlobMatcher>>);
        {
            // Track only the pattern string so unrelated config reloads don't
            // recompile the glob. The effect fires once up-front and then each
            // time the pattern changes.
            let config = common.config;
            cx.create_effect(move |prev_pattern: Option<String>| {
                let pattern = config.with(|c| c.editor.files_exclude.clone());
                if prev_pattern.as_deref() == Some(pattern.as_str()) {
                    return pattern;
                }
                let matcher = if pattern.is_empty() {
                    None
                } else {
                    match Glob::new(&pattern) {
                        Ok(g) => Some(Rc::new(g.compile_matcher())),
                        Err(e) => {
                            tracing::error!(
                                target: "files_exclude",
                                "Failed to compile glob: {e}"
                            );
                            None
                        }
                    }
                };
                files_exclude.set(matcher);
                pattern
            });
        }
        let data = Self {
            root,
            naming,
            naming_editor_data,
            common,
            scroll_to_line: cx.create_rw_signal(None),
            select: cx.create_rw_signal(None),
            starred,
            files_exclude,
        };
        if data.common.workspace.path.is_some() {
            // only fill in the child files if there is open folder
            tracing::info!("[file-explorer] Initial toggle_expand for {:?}", path);
            data.toggle_expand(&path);
        }
        data
    }

    /// Get the naming editor text as an `OsString` suitable for path operations.
    fn editor_text_as_os_string(&self) -> std::ffi::OsString {
        let rope = self.naming_editor_data.text();
        let s: String = rope.slice_to_cow(..).into_owned();
        s.into()
    }

    /// Reload the file explorer data via reading the root directory.
    /// Note that this will not update immediately.
    pub fn reload(&self) {
        let path = self.root.with_untracked(|root| root.path.clone());
        self.read_dir(&path);
    }

    /// Toggle whether the directory is expanded or not.
    /// Does nothing if the path does not exist or is not a directory.
    /// This is the core of the lazy loading strategy: directories are only read from
    /// disk the first time they are expanded. Subsequent toggles just show/hide
    /// the already-loaded children and update the open count for virtual scrolling.
    ///
    /// When the directory hasn't been read yet, we avoid updating the root signal
    /// (which would trigger a pointless empty render). Instead, we fire the async
    /// read_dir and let its callback do a single signal update with both the children
    /// and the open/count state — one render with full data.
    pub fn toggle_expand(&self, path: &Path) {
        // First, check the current state without mutating the signal.
        let state = self.root.with_untracked(|root| {
            let Some(node) = root.get_file_node(path) else {
                return None;
            };
            if !node.is_dir {
                return None;
            }
            Some((node.open, node.read))
        });

        let Some((currently_open, read)) = state else {
            return;
        };

        if read {
            // Already read: toggle open/closed and update counts in one signal update.
            self.root.update(|root| {
                if let Some(node) = root.get_file_node_mut(path) {
                    node.open = !node.open;
                }
                root.update_node_count_recursive(path);
            });
        } else if !currently_open {
            // Not yet read and currently closed: fire async read_dir which will
            // set open=true, insert children, and update counts in a single
            // signal update — avoiding a useless empty-folder render.
            self.read_dir_and_open(path);
        }
    }

    /// Read a directory from disk and open it in a single signal update.
    /// Used when expanding a directory for the first time.
    fn read_dir_and_open(&self, path: &Path) {
        tracing::info!("[file-explorer] read_dir_and_open called for {:?}", path);
        let root = self.root;
        let files_exclude = self.files_exclude;
        let send = {
            let path = path.to_path_buf();
            create_ext_action(self.common.scope, move |result| {
                let Ok(ProxyResponse::ReadDirResponse { mut items }) = result else {
                    return;
                };

                let exclude_matcher = files_exclude.get_untracked();

                // Single signal update: set open + children + counts together.
                root.update(|root| {
                    if let Some(node) = root.get_file_node_mut(&path) {
                        if let Some(ref matcher) = exclude_matcher {
                            items.retain(|i| !matcher.is_match(&i.path));
                        }

                        node.open = true;
                        node.read = true;
                        node.extend_children_sorted(items);
                    }
                    root.update_node_count_recursive(&path);
                });
            })
        };

        self.common.proxy.read_dir(path.to_path_buf(), send);
    }

    pub fn read_dir(&self, path: &Path) {
        tracing::info!("[file-explorer] read_dir called for {:?}", path);
        self.read_dir_cb(path, |_| {});
    }

    /// Read the directory's information and update the file explorer tree.
    /// `done : FnOnce(was_read: bool)` is called when the operation is completed, whether success,
    /// failure, or ignored.
    ///
    /// The proxy performs the actual filesystem read asynchronously. When results arrive:
    /// 1. Apply file exclusion globs (e.g., .git, node_modules)
    /// 2. Reconcile with existing children: remove deleted paths, keep already-read
    ///    subdirectories (and re-read them to pick up changes), add new paths
    /// 3. Recompute children_open_count for the affected subtree
    pub fn read_dir_cb(&self, path: &Path, done: impl FnOnce(bool) + 'static) {
        let root = self.root;
        let data = self.clone();
        let files_exclude = self.files_exclude;
        let send = {
            let path = path.to_path_buf();
            create_ext_action(self.common.scope, move |result| {
                match &result {
                    Ok(ProxyResponse::ReadDirResponse { items }) => {
                        tracing::info!(
                            "[file-explorer] ReadDir response received for {:?}: {} items",
                            path,
                            items.len()
                        );
                    }
                    Ok(other) => {
                        tracing::warn!(
                            "[file-explorer] ReadDir unexpected response for {:?}: {:?}",
                            path,
                            std::mem::discriminant(other)
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "[file-explorer] ReadDir error for {:?}: {:?}",
                            path,
                            e
                        );
                    }
                }
                let Ok(ProxyResponse::ReadDirResponse { mut items }) = result else {
                    done(false);
                    return;
                };

                let exclude_matcher = files_exclude.get_untracked();

                root.update(|root| {
                    if let Some(node) = root.get_file_node_mut(&path) {
                        if let Some(ref matcher) = exclude_matcher {
                            items.retain(|i| !matcher.is_match(&i.path));
                        }

                        node.read = true;

                        // Reconcile: remove paths gone from disk, add new ones,
                        // and for already-expanded subdirectories kick off a
                        // recursive re-read to pick up nested changes.
                        let new_paths: HashSet<PathBuf> =
                            items.iter().map(|i| i.path.clone()).collect();
                        node.retain_children_sorted(|path| new_paths.contains(path));
                        let mut to_reread: Vec<PathBuf> = Vec::new();
                        let mut to_insert: Vec<FileNodeItem> = Vec::new();
                        for item in items {
                            match node.children.get(&item.path) {
                                Some(existing) if existing.read => {
                                    to_reread.push(existing.path.clone());
                                }
                                Some(_) => {}
                                None => to_insert.push(item),
                            }
                        }
                        node.extend_children_sorted(to_insert);
                        for path in to_reread {
                            data.read_dir(&path);
                        }
                    }
                    root.update_node_count_recursive(&path);
                });

                done(true);
            })
        };

        // Ask the proxy for the directory information
        self.common.proxy.read_dir(path.to_path_buf(), send);
    }

    /// Returns `true` if `path` exists in the file explorer tree and is a directory, `false`
    /// otherwise.
    fn is_dir(&self, path: &Path) -> bool {
        self.root.with_untracked(|root| {
            root.get_file_node(path).is_some_and(|node| node.is_dir)
        })
    }

    /// The current path that we're renaming to / creating or duplicating a node at.  
    /// Note: returns `None` when renaming if the file name has not changed.
    fn naming_path(&self) -> Option<PathBuf> {
        self.naming.with_untracked(|naming| match naming {
            Naming::None => None,
            Naming::Renaming(_) => {
                if let RenamedPath::Renamed { new_path, .. } = self.renamed_path() {
                    Some(new_path)
                } else {
                    None
                }
            }
            Naming::NewNode(n) => {
                let relative_path = self.editor_text_as_os_string();
                Some(n.base_path.join(relative_path))
            }
            Naming::Duplicating(d) => {
                let relative_path = self.editor_text_as_os_string();
                let new_path =
                    d.path.parent().unwrap_or("".as_ref()).join(relative_path);

                Some(new_path)
            }
        })
    }

    /// If there is an in progress rename and the user has entered a path that differs from the
    /// current path, gets the current and new paths of the renamed node.
    fn renamed_path(&self) -> RenamedPath {
        self.naming.with_untracked(|naming| {
            if let Some(current_path) = naming.as_renaming().map(|x| &x.path) {
                let current_file_name = current_path.file_name().unwrap_or_default();
                // `new_relative_path` is the new path relative to the parent directory, unless the
                // user has entered an absolute path.
                let new_relative_path = self.editor_text_as_os_string();

                if new_relative_path == current_file_name {
                    RenamedPath::NameUnchanged
                } else {
                    let new_path = current_path
                        .parent()
                        .unwrap_or("".as_ref())
                        .join(new_relative_path);

                    RenamedPath::Renamed {
                        current_path: current_path.to_owned(),
                        new_path,
                    }
                }
            } else {
                RenamedPath::NotRenaming
            }
        })
    }

    /// If a naming is in progress and the user has entered a valid path, send the request to
    /// actually perform the change.
    pub fn finish_naming(&self) {
        match self.naming.get_untracked() {
            Naming::None => {}
            Naming::Renaming(_) => {
                let renamed_path = self.renamed_path();
                match renamed_path {
                    // Should not occur
                    RenamedPath::NotRenaming => {}
                    RenamedPath::NameUnchanged => {
                        self.cancel_naming();
                    }
                    RenamedPath::Renamed {
                        current_path,
                        new_path,
                    } => {
                        self.common.internal_command.send(
                            InternalCommand::FinishRenamePath {
                                current_path,
                                new_path,
                            },
                        );
                    }
                }
            }
            Naming::NewNode(n) => {
                let Some(path) = self.naming_path() else {
                    return;
                };

                self.common
                    .internal_command
                    .send(InternalCommand::FinishNewNode {
                        is_dir: n.is_dir,
                        path,
                    });
            }
            Naming::Duplicating(d) => {
                let Some(path) = self.naming_path() else {
                    return;
                };

                self.common.internal_command.send(
                    InternalCommand::FinishDuplicate {
                        source: d.path.to_owned(),
                        path,
                    },
                );
            }
        }
    }

    /// Closes the naming text box without applying the effect.
    pub fn cancel_naming(&self) {
        self.naming.set(Naming::None);
    }

    pub fn click(&self, path: &Path, config: ReadSignal<Arc<LapceConfig>>) {
        if self.is_dir(path) {
            self.toggle_expand(path);
        } else if !config.get_untracked().core.file_explorer_double_click {
            self.common
                .internal_command
                .send(InternalCommand::OpenFile {
                    path: path.to_path_buf(),
                })
        }
    }

    /// Reveals a file in the tree by expanding all ancestor directories and scrolling
    /// to the file's position. This may require multiple async read_dir calls if
    /// ancestor directories haven't been loaded yet, in which case it calls itself
    /// recursively via the read_dir_cb callback until the full path is loaded.
    pub fn reveal_in_file_tree(&self, path: PathBuf) {
        let done = self
            .root
            .try_update(|root| {
                // Fast path: the file is already loaded in the tree
                if root.get_file_node(&path).is_some() {
                    for current_path in path.ancestors() {
                        if let Some(file) = root.get_file_node_mut(current_path) {
                            if file.is_dir {
                                file.open = true;
                            }
                        }
                    }
                    root.update_node_count_recursive(&path);
                    true
                } else {
                    // Slow path: walk up the ancestors to find the deepest loaded
                    // directory, then trigger a read_dir on it. The callback will
                    // recursively call reveal_in_file_tree again until the target is found.
                    let mut read_dir = None;
                    let mut exist = false;
                    for current_path in path.ancestors() {
                        if let Some(file) = root.get_file_node_mut(current_path) {
                            exist = true;
                            if file.is_dir {
                                file.open = true;
                                if !file.read {
                                    read_dir = Some(current_path.to_path_buf())
                                }
                            }
                            break;
                        }
                    }
                    if let (true, Some(dir)) = (exist, read_dir) {
                        let explorer = self.clone();
                        let select_path = path.clone();
                        self.read_dir_cb(&dir, move |_| {
                            explorer.reveal_in_file_tree(select_path);
                        })
                    }
                    false
                }
            })
            .unwrap_or(false);
        if done {
            let starred = self.starred.get_untracked();
            let (found, line) = self
                .root
                .with_untracked(|x| x.find_file_at_line_starred(&path, &starred));
            if found {
                self.scroll_to_line.set(Some(line));
                self.select.set(Some(FileNodeViewKind::Path(path)));
            }
        }
    }

    pub fn double_click(
        &self,
        path: &Path,
        config: ReadSignal<Arc<LapceConfig>>,
    ) -> EventPropagation {
        if self.is_dir(path) {
            EventPropagation::Continue
        } else if config.get_untracked().core.file_explorer_double_click {
            self.common
                .internal_command
                .send(InternalCommand::OpenFile {
                    path: path.to_path_buf(),
                });
            EventPropagation::Stop
        } else {
            EventPropagation::Stop
        }
    }

    pub fn secondary_click(&self, path: &Path) {
        let common = self.common.clone();
        let path_a = path.to_owned();
        // TODO: should we just pass is_dir into secondary click?
        let is_dir = self.is_dir(path);

        let Some(workspace_path) = self.common.workspace.path.as_ref() else {
            // There is no context menu if we are not in a workspace
            return;
        };

        let is_workspace = path == workspace_path;

        let base_path_a = if is_dir {
            Some(path_a.clone())
        } else {
            path_a.parent().map(ToOwned::to_owned)
        };
        let base_path_a = base_path_a.as_ref().unwrap_or(workspace_path);

        let mut menu = Menu::new("");

        let base_path = base_path_a.clone();
        let data = self.clone();
        let naming = self.naming;
        menu = menu.entry(MenuItem::new("New File").action(move || {
            let base_path_b = &base_path;
            let base_path = base_path.clone();
            data.read_dir_cb(base_path_b, move |was_read| {
                if !was_read {
                    tracing::warn!(
                        "Failed to read directory, avoiding creating node in: {:?}",
                        base_path
                    );
                    return;
                }

                naming.set(Naming::NewNode(NewNode {
                    state: NamingState::Naming,
                    base_path: base_path.clone(),
                    is_dir: false,
                    editor_needs_reset: true,
                }));
            });
        }));

        let base_path = base_path_a.clone();
        let data = self.clone();
        let naming = self.naming;
        menu = menu.entry(MenuItem::new("New Directory").action(move || {
            let base_path_b = &base_path;
            let base_path = base_path.clone();
            data.read_dir_cb(base_path_b, move |was_read| {
                if !was_read {
                    tracing::warn!(
                        "Failed to read directory, avoiding creating node in: {:?}",
                        base_path
                    );
                    return;
                }

                naming.set(Naming::NewNode(NewNode {
                    state: NamingState::Naming,
                    base_path: base_path.clone(),
                    is_dir: true,
                    editor_needs_reset: true,
                }));
            })
        }));

        menu = menu.separator();

        {
            let path = path_a.clone();
            #[cfg(not(target_os = "macos"))]
            let title = "Reveal in system file explorer";
            #[cfg(target_os = "macos")]
            let title = "Reveal in Finder";
            menu = menu.entry(MenuItem::new(title).action(move || {
                let path = path.parent().unwrap_or(&path);
                if !path.exists() {
                    return;
                }

                if let Err(err) = open::that(path) {
                    tracing::error!(
                        "Failed to reveal file in system file explorer: {}",
                        err
                    );
                }
            }));
        }

        if !is_workspace {
            let path = path_a.clone();
            menu = menu.entry(MenuItem::new("Rename").action(move || {
                naming.set(Naming::Renaming(Renaming {
                    state: NamingState::Naming,
                    path: path.clone(),
                    editor_needs_reset: true,
                }));
            }));

            let path = path_a.clone();
            menu = menu.entry(MenuItem::new("Duplicate").action(move || {
                naming.set(Naming::Duplicating(Duplicating {
                    state: NamingState::Naming,
                    path: path.clone(),
                    editor_needs_reset: true,
                }));
            }));

            // TODO: it is common for shift+right click to make 'Move file to trash' an actual
            // Delete, which can be useful for large files.
            let path = path_a.clone();
            let proxy = common.proxy.clone();
            let trash_text = if is_dir {
                "Move Directory to Trash"
            } else {
                "Move File to Trash"
            };
            menu = menu.entry(MenuItem::new(trash_text).action(move || {
                proxy.trash_path(path.clone(), |res| {
                    if let Err(err) = res {
                        tracing::warn!("Failed to trash path: {:?}", err);
                    }
                })
            }));
        }

        menu = menu.separator();

        let path = path_a.clone();
        menu = menu.entry(MenuItem::new("Copy Path").action(move || {
            let mut clipboard = SystemClipboard::new();
            clipboard.put_string(path.to_string_lossy());
        }));

        let path = path_a.clone();
        let workspace = common.workspace.clone();
        menu = menu.entry(MenuItem::new("Copy Relative Path").action(move || {
            let relative_path = if let Some(workspace_path) = &workspace.path {
                path.strip_prefix(workspace_path).unwrap_or(&path)
            } else {
                path.as_ref()
            };

            let mut clipboard = SystemClipboard::new();
            clipboard.put_string(relative_path.to_string_lossy());
        }));

        menu = menu.separator();

        // Exclude / include path toggle
        if !is_workspace {
            let config = self.common.config;
            let excluded_paths = config.get_untracked().core.excluded_paths.clone();
            let rel_path = path_a
                .strip_prefix(workspace_path)
                .ok()
                .map(|r| r.to_string_lossy().to_string());

            if let Some(rel) = rel_path {
                let is_currently_excluded = excluded_paths.contains(&rel);
                let label = if is_currently_excluded {
                    if is_dir {
                        "Remove Directory from Excluded Paths"
                    } else {
                        "Remove File from Excluded Paths"
                    }
                } else if is_dir {
                    "Add Directory to Excluded Paths"
                } else {
                    "Add File to Excluded Paths"
                };

                let internal_command = common.internal_command;
                menu = menu.entry(MenuItem::new(label).action(move || {
                    let mut paths = excluded_paths.clone();
                    if is_currently_excluded {
                        paths.retain(|p| p != &rel);
                    } else {
                        paths.push(rel.clone());
                    }
                    let mut arr = toml_edit::Array::new();
                    for p in &paths {
                        arr.push(p.as_str());
                    }
                    LapceConfig::update_file(
                        "core",
                        "excluded-paths",
                        toml_edit::Value::Array(arr),
                    );
                    internal_command.send(InternalCommand::ReloadConfig);
                }));
            }
        }

        menu = menu.separator();

        let internal_command = common.internal_command;
        menu = menu.entry(MenuItem::new("Refresh").action(move || {
            internal_command.send(InternalCommand::ReloadFileExplorer);
        }));

        show_context_menu(menu, None);
    }

    pub fn middle_click(&self, path: &Path) -> EventPropagation {
        if self.is_dir(path) {
            EventPropagation::Continue
        } else {
            self.common
                .internal_command
                .send(InternalCommand::OpenFile {
                    path: path.to_path_buf(),
                });
            EventPropagation::Stop
        }
    }

    /// Toggle the starred state of a first-level folder.
    pub fn toggle_star(&self, path: &Path) {
        self.starred.update(|set| {
            if !set.remove(path) {
                set.insert(path.to_path_buf());
            }
        });
    }

    /// Returns the current set of starred folder paths.
    pub fn starred_folders(&self) -> Vec<PathBuf> {
        self.starred
            .with_untracked(|set| set.iter().cloned().collect())
    }
}
