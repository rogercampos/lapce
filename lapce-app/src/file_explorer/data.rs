use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ffi::OsStr,
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
use globset::Glob;
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
        });
        let naming = cx.create_rw_signal(Naming::None);
        let naming_editor_data = editors.make_local(cx, common.clone());
        let starred =
            cx.create_rw_signal(initial_starred.into_iter().collect::<HashSet<_>>());
        let data = Self {
            root,
            naming,
            naming_editor_data,
            common,
            scroll_to_line: cx.create_rw_signal(None),
            select: cx.create_rw_signal(None),
            starred,
        };
        if data.common.workspace.path.is_some() {
            // only fill in the child files if there is open folder
            tracing::info!("[file-explorer] Initial toggle_expand for {:?}", path);
            data.toggle_expand(&path);
        }
        data
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
    pub fn toggle_expand(&self, path: &Path) {
        let Some(Some(read)) = self.root.try_update(|root| {
            let read = if let Some(node) = root.get_file_node_mut(path) {
                if !node.is_dir {
                    return None;
                }
                node.open = !node.open;
                Some(node.read)
            } else {
                None
            };

            // Only update counts if the directory was already read (children exist).
            // If not yet read, counts will be updated when read_dir completes.
            if Some(true) == read {
                root.update_node_count_recursive(path);
            }
            read
        }) else {
            return;
        };

        // Read the directory's files if they haven't been read yet
        if !read {
            self.read_dir(path);
        }
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
        let config = self.common.config;
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

                root.update(|root| {
                    if let Some(node) = root.get_file_node_mut(&path) {
                        // TODO: do not recreate glob every time we read a directory
                        // Retain only items that are not excluded from view by the configuration
                        match Glob::new(&config.get().editor.files_exclude) {
                            Ok(glob) => {
                                let matcher = glob.compile_matcher();
                                items.retain(|i| !matcher.is_match(&i.path));
                            }
                            Err(e) => tracing::error!(
                                target:"files_exclude",
                                "Failed to compile glob: {}",
                                e
                            ),
                        }

                        node.read = true;

                        // Remove paths that no longer exist on disk
                        let removed_paths: Vec<PathBuf> = node
                            .children
                            .keys()
                            .filter(|p| !items.iter().any(|i| &&i.path == p))
                            .map(PathBuf::from)
                            .collect();
                        for path in removed_paths {
                            node.children.remove(&path);
                        }

                        // For existing children that were already read (expanded dirs),
                        // re-read them recursively to pick up any nested changes.
                        // For new children, insert them into the tree.
                        for item in items {
                            if let Some(existing) = node.children.get(&item.path) {
                                if existing.read {
                                    data.read_dir(&existing.path);
                                }
                            } else {
                                node.children.insert(item.path.clone(), item);
                            }
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
                let relative_path = self.naming_editor_data.text();
                let relative_path: Cow<OsStr> = match relative_path.slice_to_cow(..)
                {
                    Cow::Borrowed(path) => Cow::Borrowed(path.as_ref()),
                    Cow::Owned(path) => Cow::Owned(path.into()),
                };

                Some(n.base_path.join(relative_path))
            }
            Naming::Duplicating(d) => {
                let relative_path = self.naming_editor_data.text();
                let relative_path: Cow<OsStr> = match relative_path.slice_to_cow(..)
                {
                    Cow::Borrowed(path) => Cow::Borrowed(path.as_ref()),
                    Cow::Owned(path) => Cow::Owned(path.into()),
                };

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
                let new_relative_path = self.naming_editor_data.text();

                let new_relative_path: Cow<OsStr> =
                    match new_relative_path.slice_to_cow(..) {
                        Cow::Borrowed(path) => Cow::Borrowed(path.as_ref()),
                        Cow::Owned(path) => Cow::Owned(path.into()),
                    };

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
                .send(InternalCommand::OpenFileInNewTab {
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
