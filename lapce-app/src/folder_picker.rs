use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use floem::{
    View,
    ext_event::create_ext_action,
    keyboard::Modifiers,
    peniko::Color,
    reactive::{
        Memo, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
    },
    style::{AlignItems, CursorStyle},
    views::{
        Decorators, VirtualVector, container, label, scroll,
        scroll::PropagatePointerWheel, stack, svg, virtual_stack,
    },
};
use lapce_core::{command::FocusCommand, mode::Mode, selection::Selection};
use lapce_rpc::{file::FileNodeItem, proxy::ProxyResponse};
use lapce_xi_rope::Rope;

use crate::{
    about::exclusive_popup,
    command::{CommandExecuted, CommandKind, LapceCommand},
    config::{
        LapceConfig, color::LapceColor, icon::LapceIcons, layout::LapceLayout,
    },
    editor::EditorData,
    keypress::KeyPressFocus,
    main_split::MainSplitData,
    resizable_container::resizable_container,
    text_input::TextInputBuilder,
    workspace_data::{CommonData, Focus, WorkspaceData},
};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// A view item produced by flattening the folder tree for virtual_stack.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FolderNodeViewData {
    path: PathBuf,
    open: bool,
    level: usize,
    /// Whether this node is a direct match for the current filter
    /// (as opposed to just being an ancestor of a match).
    is_match: bool,
    /// Whether this node has child directories. When false, the
    /// expand/collapse arrow is hidden.
    has_children: bool,
}

/// Data model for the folder picker modal. Shows a tree of workspace directories
/// (matching the file explorer look) and lets the user select one.
///
/// When the search input is empty the full tree is shown (lazy-loaded,
/// expand/collapse). When search text is entered the tree is pruned to show
/// only matching folders and their ancestors.
#[derive(Clone)]
pub struct FolderPickerData {
    pub visible: RwSignal<bool>,
    /// The root of the directory-only tree (lazy-loaded for the visual tree).
    pub root: RwSignal<FileNodeItem>,
    /// Editor for the search/filter input at the top.
    pub input_editor: EditorData,
    /// Derived from input_editor's buffer – the current filter string.
    pub filter_text: RwSignal<String>,
    /// All relative folder paths in the workspace. Populated once via the
    /// `ListAllFolders` RPC (a fast filesystem walk) when the picker opens.
    pub all_folder_paths: RwSignal<Vec<PathBuf>>,
    /// Folders matching the current `filter_text` (empty when no filter).
    pub filtered_paths: Memo<Vec<PathBuf>>,
    /// The set of paths that should be visible in the filtered tree view.
    /// Includes the matched folders **plus** all their ancestor folders so the
    /// tree structure is preserved.
    pub filtered_tree_visible: Memo<std::collections::HashSet<PathBuf>>,
    /// Pre-computed set of relative paths that are parents of at least one
    /// other folder.  Depends only on `all_folder_paths` — recomputed once
    /// when the folder list changes, NOT on every scroll or filter keystroke.
    pub parent_set: Memo<std::collections::HashSet<PathBuf>>,
    /// Pre-computed flat list of items for the **filtered** tree view.
    /// Recomputed only when filter-related signals change (filtered_paths,
    /// filtered_tree_visible, filtered_open_state, parent_set), NOT on
    /// scroll / viewport changes.
    pub(crate) filtered_items: Memo<Vec<FolderNodeViewData>>,
    pub common: Rc<CommonData>,
    /// The focus state that was active before the picker opened.
    /// Restored when the picker closes.
    previous_focus: RwSignal<Focus>,
    /// Callback invoked when a folder is confirmed. `None` means "clear filter".
    pub on_confirm: RwSignal<Option<Rc<dyn Fn(Option<PathBuf>)>>>,
    /// User overrides for open/closed state in the filtered view.
    /// Key = relative path, Value = open or closed.
    /// Reset whenever `filter_text` changes.
    pub filtered_open_state: RwSignal<HashMap<PathBuf, bool>>,
}

impl std::fmt::Debug for FolderPickerData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderPickerData").finish()
    }
}

impl FolderPickerData {
    pub fn new(
        cx: Scope,
        main_split: MainSplitData,
        common: Rc<CommonData>,
    ) -> Self {
        let visible = cx.create_rw_signal(false);
        let workspace_path = common.workspace.path.clone().unwrap_or_default();
        let root = cx.create_rw_signal(FileNodeItem {
            path: workspace_path.clone(),
            is_dir: true,
            read: false,
            open: false,
            children: HashMap::new(),
            children_open_count: 0,
        });
        let on_confirm: RwSignal<Option<Rc<dyn Fn(Option<PathBuf>)>>> =
            cx.create_rw_signal(None);
        let previous_focus = cx.create_rw_signal(Focus::Workbench);
        let input_editor = main_split.editors.make_local(cx, common.clone());

        // Track filter text from the input editor's buffer.
        let filter_text = cx.create_rw_signal(String::new());
        {
            let buffer = input_editor.doc().buffer;
            cx.create_effect(move |_| {
                let content = buffer.with(|b| b.to_string());
                filter_text.set(content);
            });
        }

        // All folder paths — populated via ListAllFolders RPC when the
        // picker opens.  This is a simple RwSignal, not a Memo.
        let all_folder_paths: RwSignal<Vec<PathBuf>> =
            cx.create_rw_signal(Vec::new());

        // User open/close overrides for filtered view. Reset when filter changes.
        let filtered_open_state: RwSignal<HashMap<PathBuf, bool>> =
            cx.create_rw_signal(HashMap::new());
        {
            cx.create_effect(move |_| {
                // Subscribe to filter_text changes.
                let _ = filter_text.get();
                // Reset overrides — each new filter starts with default state.
                filtered_open_state.set(HashMap::new());
            });
        }

        // Filter paths when filter_text is non-empty.
        //
        // Without `/`: the input is a case-insensitive literal substring
        // match against each path component (folder name).  For example,
        // "expenses" matches only folders whose name contains "expenses",
        // NOT "universal_exports".
        //
        // With `/`: each segment between slashes is a literal substring
        // and segments are joined with `.*/.*` so that a real `/` must
        // appear between them.
        // Example: `fronte/exp` → `(?i)fronte.*/.*exp`
        // This matches paths like `frontend/app/expenses`.
        let filtered_paths = cx.create_memo(move |_| {
            let all = all_folder_paths.get();
            let input = filter_text.get();
            if input.is_empty() {
                return Vec::new();
            }

            if input.contains('/') {
                // Slash mode: regex with literal segments joined by `.*/.*`.
                let regex_pattern: String = input
                    .split('/')
                    .map(|seg| regex::escape(seg))
                    .collect::<Vec<_>>()
                    .join(".*/.*");
                let re = match regex::RegexBuilder::new(&regex_pattern)
                    .case_insensitive(true)
                    .build()
                {
                    Ok(re) => re,
                    Err(_) => return Vec::new(),
                };
                all.into_iter()
                    .filter(|path| {
                        let display = path.to_string_lossy();
                        re.is_match(&display)
                    })
                    .collect()
            } else {
                // No slash: case-insensitive literal substring match.
                let needle = input.to_lowercase();
                all.into_iter()
                    .filter(|path| {
                        let display = path.to_string_lossy();
                        display.to_lowercase().contains(&needle)
                    })
                    .collect()
            }
        });

        // Build the set of tree-visible paths: the matched folders plus
        // all their ancestor prefixes, so the tree structure is preserved.
        let filtered_tree_visible = cx.create_memo(move |_| {
            let matched = filtered_paths.get();
            let mut visible = std::collections::HashSet::new();
            for path in &matched {
                // Add the matched path itself.
                visible.insert(path.clone());
                // Add every ancestor prefix.
                let mut ancestor = path.as_path();
                while let Some(parent) = ancestor.parent() {
                    if parent.as_os_str().is_empty() {
                        break;
                    }
                    visible.insert(parent.to_path_buf());
                    ancestor = parent;
                }
            }
            visible
        });

        // Pre-compute the set of relative paths that have at least one child
        // folder.  Only depends on `all_folder_paths`, so it's recomputed once
        // when the folder list is loaded — not on every keystroke or scroll.
        let parent_set = cx.create_memo(move |_| {
            let all = all_folder_paths.get();
            let mut parents = std::collections::HashSet::new();
            for p in &all {
                if let Some(parent) = p.parent() {
                    if !parent.as_os_str().is_empty() {
                        parents.insert(parent.to_path_buf());
                    }
                }
            }
            parents
        });

        // Pre-compute the flat item list for the filtered tree view.  Depends
        // on filter-related signals only — NOT on the viewport / scroll
        // position.  The `virtual_stack` data closure just reads this Memo,
        // so scrolling is essentially free.
        let workspace_for_memo = common.workspace.path.clone().unwrap_or_default();
        let filtered_items: Memo<Vec<FolderNodeViewData>> =
            cx.create_memo(move |_| {
                let visible_set = filtered_tree_visible.get();
                if visible_set.is_empty() {
                    return Vec::new();
                }
                let matched = filtered_paths.get();
                let overrides = filtered_open_state.get();
                let all = all_folder_paths.get();
                let parents = parent_set.get();

                build_filtered_items(
                    &workspace_for_memo,
                    &visible_set,
                    &matched,
                    &overrides,
                    &all,
                    &parents,
                )
            });

        // Auto-close when focus changes away from FolderPicker
        {
            let focus = common.focus;
            cx.create_effect(move |_| {
                let f = focus.get();
                if f != Focus::FolderPicker && visible.get_untracked() {
                    visible.set(false);
                }
            });
        }

        Self {
            visible,
            root,
            input_editor,
            filter_text,
            all_folder_paths,
            filtered_paths,
            filtered_tree_visible,
            parent_set,
            filtered_items,
            common,
            previous_focus,
            on_confirm,
            filtered_open_state,
        }
    }

    /// Open the folder picker. `callback` is invoked when the user confirms a
    /// selection (`Some(path)`) or clears the filter (`None`).
    pub fn open(&self, callback: impl Fn(Option<PathBuf>) + 'static) {
        // Clear search input
        self.input_editor.doc().reload(Rope::from(""), true);
        self.input_editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::caret(0)));

        // Remember the current focus so we can restore it on close.
        self.previous_focus
            .set(self.common.focus.get_untracked().clone());
        self.on_confirm.set(Some(Rc::new(callback)));

        // If root hasn't been read yet, trigger a read for the tree view.
        let need_read = self.root.with_untracked(|r| !r.read);
        if need_read {
            let workspace_path =
                self.common.workspace.path.clone().unwrap_or_default();
            self.read_dir(&workspace_path);
        }

        // Expand root if not already
        self.root.update(|r| {
            if !r.open {
                r.open = true;
            }
        });

        // Fetch all folder paths via a fast filesystem walk (for filtering).
        self.load_all_folders();

        self.visible.set(true);
        self.common.focus.set(Focus::FolderPicker);
    }

    pub fn close(&self) {
        self.visible.set(false);
        if self.common.focus.get_untracked() == Focus::FolderPicker {
            let prev = self.previous_focus.get_untracked();
            self.common.focus.set(prev);
        }
    }

    /// Select a specific folder path and confirm.
    pub fn confirm_path(&self, path: PathBuf) {
        let workspace_path = self.common.workspace.path.clone().unwrap_or_default();
        let relative = path
            .strip_prefix(&workspace_path)
            .unwrap_or(&path)
            .to_path_buf();
        if let Some(cb) = self.on_confirm.get_untracked() {
            cb(Some(relative));
        }
        self.close();
    }

    /// Clear the folder filter (search all files).
    pub fn clear_selection(&self) {
        if let Some(cb) = self.on_confirm.get_untracked() {
            cb(None);
        }
        self.close();
    }

    /// Fetch all workspace folder paths via the `ListAllFolders` RPC.
    /// The result populates `all_folder_paths` for search/filtering.
    fn load_all_folders(&self) {
        let all_folder_paths = self.all_folder_paths;
        let send = create_ext_action(self.common.scope, move |result| {
            if let Ok(ProxyResponse::ListAllFoldersResponse { folders }) = result {
                all_folder_paths.set(folders);
            }
        });
        self.common.proxy.list_all_folders(send);
    }

    /// Toggle expand/collapse of a directory node in the **filtered** view.
    /// `rel_path` is the relative path of the node to toggle.
    pub fn toggle_filtered_expand(&self, rel_path: &Path) {
        self.filtered_open_state.update(|state| {
            let current = state.get(rel_path).copied();
            match current {
                Some(open) => {
                    state.insert(rel_path.to_path_buf(), !open);
                }
                None => {
                    // No override yet. Check whether this is a matched folder
                    // (default closed) or an ancestor (default open).
                    let is_match = self
                        .filtered_paths
                        .with_untracked(|paths| paths.iter().any(|p| p == rel_path));
                    // Default is: ancestors open, matches closed.
                    // So toggling flips it.
                    let new_open = if is_match { true } else { false };
                    state.insert(rel_path.to_path_buf(), new_open);
                }
            }
        });
    }

    /// Toggle expand/collapse of a directory node in the **unfiltered** tree.
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

            if Some(true) == read {
                root.update_node_count_recursive(path);
            }
            read
        }) else {
            return;
        };

        if !read {
            self.read_dir(path);
        }
    }

    /// Read a directory from the proxy and populate the tree (directories only).
    /// Only reads one level — child directories are loaded lazily on expand.
    fn read_dir(&self, path: &Path) {
        let root_signal = self.root;
        let data = self.clone();
        let path = path.to_path_buf();
        let config = self.common.config;

        let send = {
            let path = path.clone();
            create_ext_action(self.common.scope, move |result| {
                let Ok(ProxyResponse::ReadDirResponse { mut items }) = result else {
                    return;
                };

                // Filter to directories only
                items.retain(|item| item.is_dir);

                // Apply file exclusion globs
                if let Ok(glob) =
                    globset::Glob::new(&config.get_untracked().editor.files_exclude)
                {
                    let matcher = glob.compile_matcher();
                    items.retain(|i| !matcher.is_match(&i.path));
                }

                root_signal.update(|root| {
                    if let Some(node) = root.get_file_node_mut(&path) {
                        node.read = true;
                        node.open = true;

                        // Remove paths that no longer exist
                        let removed: Vec<PathBuf> = node
                            .children
                            .keys()
                            .filter(|p| !items.iter().any(|i| &&i.path == p))
                            .cloned()
                            .collect();
                        for p in removed {
                            node.children.remove(&p);
                        }

                        // Add new children, re-read existing
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
            })
        };

        self.common.proxy.read_dir(path, send);
    }
}

impl KeyPressFocus for FolderPickerData {
    fn get_mode(&self) -> Mode {
        Mode::Insert
    }

    fn check_condition(
        &self,
        condition: crate::keypress::condition::Condition,
    ) -> bool {
        matches!(condition, crate::keypress::condition::Condition::ModalFocus)
    }

    fn run_command(
        &self,
        command: &LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Focus(cmd) => match cmd {
                FocusCommand::ModalClose => self.close(),
                _ => return CommandExecuted::No,
            },
            _ => {
                self.input_editor.run_command(command, count, mods);
            }
        }
        CommandExecuted::Yes
    }

    fn receive_char(&self, c: &str) {
        self.input_editor.receive_char(c);
    }

    fn focus_only(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Virtual list adapters
// ---------------------------------------------------------------------------

/// Adapter for the **unfiltered** tree view.  Walks the lazy-loaded
/// `FileNodeItem` tree, respecting the `open` flag on each node.
struct FolderTreeVirtualList {
    root: FileNodeItem,
}

impl FolderTreeVirtualList {
    /// Recursively flatten open nodes into view items within the given range.
    fn flatten_into(
        &self,
        node: &FileNodeItem,
        items: &mut Vec<FolderNodeViewData>,
        min: usize,
        max: usize,
        current: usize,
        level: usize,
    ) -> usize {
        if current > max {
            return current;
        }

        if current >= min {
            // has_children: true if the node hasn't been read yet (could have
            // children) or if it has at least one directory child.
            let has_children =
                !node.read || node.children.values().any(|child| child.is_dir);
            items.push(FolderNodeViewData {
                path: node.path.clone(),
                open: node.open,
                level,
                is_match: false,
                has_children,
            });
        }

        if !node.open {
            return current;
        }

        let mut i = current;
        let mut children = node.children.values().collect::<Vec<_>>();
        children.sort();
        for child in children {
            if !child.is_dir {
                continue;
            }
            i = self.flatten_into(child, items, min, max, i + 1, level + 1);
            if i > max {
                return i;
            }
        }
        i
    }
}

impl VirtualVector<FolderNodeViewData> for FolderTreeVirtualList {
    fn total_len(&self) -> usize {
        self.root.children_open_count + 1
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = FolderNodeViewData> {
        let mut items = Vec::new();
        self.flatten_into(&self.root, &mut items, range.start, range.end, 0, 1);
        items.into_iter()
    }
}

/// Adapter for the **filtered** tree view.  Wraps a pre-computed item list
/// (built by the `filtered_items` Memo in `FolderPickerData`).
struct FilteredFolderVirtualList {
    items: Vec<FolderNodeViewData>,
}

impl VirtualVector<FolderNodeViewData> for FilteredFolderVirtualList {
    fn total_len(&self) -> usize {
        self.items.len()
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = FolderNodeViewData> {
        let start = range.start.min(self.items.len());
        let end = range.end.min(self.items.len());
        self.items[start..end].to_vec().into_iter()
    }
}

/// Build the flat item list for the filtered tree view.
///
/// Extracted as a free function so it can be called from a `Memo` (which
/// runs only when its tracked signals change, decoupled from the viewport).
fn build_filtered_items(
    workspace: &Path,
    visible: &std::collections::HashSet<PathBuf>,
    matched: &[PathBuf],
    open_overrides: &HashMap<PathBuf, bool>,
    all_folder_paths: &[PathBuf],
    parent_set: &std::collections::HashSet<PathBuf>,
) -> Vec<FolderNodeViewData> {
    let matched_set: std::collections::HashSet<&PathBuf> = matched.iter().collect();

    // Build the full set of paths to consider: visible set + immediate
    // children of any node the user has explicitly opened.
    let mut display_set: std::collections::HashSet<PathBuf> = visible.clone();

    // Only do the children scan if there are actually open overrides.
    let open_overrides_set: std::collections::HashSet<&Path> = open_overrides
        .iter()
        .filter(|&(_, &is_open)| is_open)
        .map(|(rel, _)| rel.as_path())
        .collect();
    if !open_overrides_set.is_empty() {
        for folder in all_folder_paths {
            if let Some(parent) = folder.parent() {
                if open_overrides_set.contains(parent)
                    && !display_set.contains(folder)
                {
                    display_set.insert(folder.clone());
                }
            }
        }
    }

    // Determine effective open state for each path.
    // Default: matched folders closed, everything else open.
    // Overrides take precedence.
    let is_open_fn = |rel: &PathBuf| -> bool {
        if let Some(&overridden) = open_overrides.get(rel) {
            return overridden;
        }
        // Default: matches are closed, ancestors are open.
        !matched_set.contains(rel)
    };

    // Sort all display paths alphabetically for tree order.
    let mut sorted: Vec<PathBuf> = display_set.into_iter().collect();
    sorted.sort();

    // Build items, skipping any path whose parent is effectively closed.
    // We track which paths are "reachable" — a path is reachable if all
    // its ancestors are open.
    let mut closed_ancestor: Option<PathBuf> = None;
    let mut items: Vec<FolderNodeViewData> = Vec::new();

    for rel in &sorted {
        // Check if this path is hidden because an ancestor is closed.
        if let Some(ref closed) = closed_ancestor {
            if rel.starts_with(closed) {
                continue;
            } else {
                // We've moved past the closed subtree.
                closed_ancestor = None;
            }
        }

        let level = rel.components().count() + 1;
        let is_match = matched_set.contains(rel);
        let open = is_open_fn(rel);
        let has_children = parent_set.contains(rel.as_path());

        items.push(FolderNodeViewData {
            path: workspace.join(rel),
            open,
            level,
            is_match,
            has_children,
        });

        if !open {
            // Everything under this path should be skipped.
            closed_ancestor = Some(rel.clone());
        }
    }

    items
}
/// Wrapper enum so `virtual_stack` gets a single concrete type.
enum FolderVirtualList {
    Tree(FolderTreeVirtualList),
    Filtered(FilteredFolderVirtualList),
}

impl VirtualVector<FolderNodeViewData> for FolderVirtualList {
    fn total_len(&self) -> usize {
        match self {
            Self::Tree(t) => t.total_len(),
            Self::Filtered(f) => f.total_len(),
        }
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = FolderNodeViewData> {
        let items: Vec<FolderNodeViewData> = match self {
            Self::Tree(t) => t.slice(range).collect(),
            Self::Filtered(f) => f.slice(range).collect(),
        };
        items.into_iter()
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn folder_picker_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.palettes.folder_picker.clone();
    let config = workspace_data.common.config;
    let visibility = data.visible;
    let close_data = data.clone();

    exclusive_popup(
        config,
        visibility,
        move || close_data.close(),
        move || folder_picker_content(workspace_data),
    )
    .debug_name("Folder Picker Popup")
}

fn folder_picker_content(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.palettes.folder_picker.clone();
    let config = workspace_data.common.config;
    let focus = workspace_data.common.focus;
    let ui_line_height = workspace_data.common.ui_line_height;

    let content = stack((
        // Title
        label(|| "Select Folder".to_string()).style(move |s| {
            let config = config.get();
            s.padding_vert(10.0)
                .padding_horiz(15.0)
                .font_bold()
                .color(config.color(LapceColor::EDITOR_FOREGROUND))
        }),
        // Search input
        folder_picker_input(data.clone(), config, focus),
        // Folder list – tree view, filtered when search text is present
        folder_list_view(data.clone(), config, ui_line_height),
        // Bottom buttons
        folder_picker_buttons(data, config),
    ))
    .style(move |s| {
        let config = config.get();
        s.flex_col()
            .size_full()
            .border(1.0)
            .border_radius(LapceLayout::BORDER_RADIUS)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::PALETTE_BACKGROUND))
    });

    resizable_container(500.0, 450.0, 300.0, 200.0, content)
}

fn folder_picker_input(
    data: FolderPickerData,
    config: ReadSignal<Arc<LapceConfig>>,
    focus: RwSignal<Focus>,
) -> impl View {
    let is_focused = move || focus.get() == Focus::FolderPicker;
    let input = TextInputBuilder::new()
        .is_focused(is_focused)
        .build_editor(data.input_editor.clone())
        .placeholder(|| "Filter folders...".to_owned())
        .style(|s| s.width_full());

    container(container(input).style(move |s| {
        let config = config.get();
        s.width_full()
            .height(30.0)
            .items_center()
            .border_bottom(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
    }))
    .style(|s| s.padding_bottom(5.0))
}

/// Shows the folder tree, optionally pruned by the current filter.
fn folder_list_view(
    data: FolderPickerData,
    config: ReadSignal<Arc<LapceConfig>>,
    ui_line_height: Memo<f64>,
) -> impl View {
    folder_tree_view(data, config, ui_line_height)
        .style(|s| s.width_full().min_height(0.0).flex_grow(1.0))
}

/// The tree view, optionally filtered by the current search text.
fn folder_tree_view(
    data: FolderPickerData,
    config: ReadSignal<Arc<LapceConfig>>,
    ui_line_height: Memo<f64>,
) -> impl View {
    let root = data.root;
    let filter_text = data.filter_text;
    let filtered_items = data.filtered_items;

    scroll(
        virtual_stack(
            move || {
                let has_filter = !filter_text.with(|t| t.is_empty());
                if has_filter {
                    // Read the pre-computed Memo — cheap clone of a Vec.
                    // The expensive filtering/sorting/tree-building already
                    // happened inside the Memo, not here.
                    let items = filtered_items.get();
                    FolderVirtualList::Filtered(FilteredFolderVirtualList { items })
                } else {
                    FolderVirtualList::Tree(FolderTreeVirtualList {
                        root: root.get(),
                    })
                }
            },
            move |node| {
                (
                    node.path.clone(),
                    node.open,
                    node.level,
                    node.is_match,
                    node.has_children,
                )
            },
            move |node| {
                let level = node.level;
                let open = node.open;
                let is_match = node.is_match;
                let has_children = node.has_children;
                let path = node.path.clone();
                let toggle_path = path.clone();
                let select_path = path.clone();
                let toggle_data = data.clone();
                let select_data = data.clone();
                let ws = data.common.workspace.path.clone().unwrap_or_default();

                let is_root = level == 1;

                let row = stack((
                    // Expand/collapse arrow — hidden for childless folders
                    svg(move || {
                        let config = config.get();
                        let svg_str = if open {
                            LapceIcons::ITEM_OPENED
                        } else {
                            LapceIcons::ITEM_CLOSED
                        };
                        config.ui_svg(svg_str)
                    })
                    .on_click_stop({
                        let toggle_path = toggle_path.clone();
                        let toggle_data = toggle_data.clone();
                        let ws = ws.clone();
                        move |_| {
                            let has_filter = !toggle_data
                                .filter_text
                                .with_untracked(|t| t.is_empty());
                            if has_filter {
                                if let Ok(rel) = toggle_path.strip_prefix(&ws) {
                                    toggle_data.toggle_filtered_expand(rel);
                                }
                            } else {
                                toggle_data.toggle_expand(&toggle_path);
                            }
                        }
                    })
                    .style(move |s| {
                        let config = config.get();
                        let size = config.ui.icon_size() as f32;
                        s.size(size, size)
                            .flex_shrink(0.0)
                            .margin_left(4.0)
                            .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                            .cursor(CursorStyle::Pointer)
                            .apply_if(!has_children, |s| {
                                s.color(Color::TRANSPARENT)
                                    .cursor(CursorStyle::Default)
                            })
                    }),
                    // Directory icon
                    svg(move || {
                        let config = config.get();
                        let svg_str = if open {
                            LapceIcons::DIRECTORY_OPENED
                        } else {
                            LapceIcons::DIRECTORY_CLOSED
                        };
                        config.ui_svg(svg_str)
                    })
                    .style(move |s| {
                        let config = config.get();
                        let base_size = config.ui.icon_size() as f32;
                        let size = (base_size * 1.25).round();
                        s.size(size, size)
                            .flex_shrink(0.0)
                            .margin_horiz(6.0)
                            .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    }),
                    // Folder name
                    label({
                        let path = path.clone();
                        move || {
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string()
                        }
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.color(config.color(LapceColor::EDITOR_FOREGROUND))
                            .text_ellipsis()
                            .min_width(0.0)
                            .apply_if(is_root, |s| s.font_bold())
                            .apply_if(is_match, |s| s.font_bold())
                    }),
                ))
                .style(move |s| {
                    s.padding_right(15.0)
                        .min_width_full()
                        .padding_left((level * 16) as f32)
                        .margin_horiz(4.0)
                        .border_radius(4.0)
                        .align_items(AlignItems::Center)
                        .hover(|s| {
                            s.background(
                                config
                                    .get()
                                    .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                            .cursor(CursorStyle::Pointer)
                        })
                });

                // Click the row (not the arrow) to select and confirm
                row.on_click_stop(move |_| {
                    select_data.confirm_path(select_path.clone());
                })
            },
        )
        .item_size_fixed(move || ui_line_height.get())
        .style(|s| s.absolute().flex_col().min_width_full()),
    )
    .style(|s| {
        s.size_full()
            .line_height(LapceLayout::UI_LINE_HEIGHT as f32)
            .set(PropagatePointerWheel, false)
    })
    .scroll_style(|s| s.hide_bars(true))
}

fn folder_picker_buttons(
    data: FolderPickerData,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let clear_data = data.clone();
    let cancel_data = data;

    container(
        stack((
            // "Clear" button — removes the folder filter
            label(|| "Clear".to_string())
                .on_click_stop(move |_| {
                    clear_data.clear_selection();
                })
                .style(move |s| {
                    let config = config.get();
                    s.padding_vert(5.0)
                        .padding_horiz(15.0)
                        .border(1.0)
                        .border_radius(LapceLayout::BORDER_RADIUS)
                        .border_color(config.color(LapceColor::LAPCE_BORDER))
                        .color(config.color(LapceColor::EDITOR_FOREGROUND))
                        .cursor(CursorStyle::Pointer)
                        .hover(|s| {
                            s.background(
                                config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                        })
                }),
            // "Cancel" button — closes without changing anything
            label(|| "Cancel".to_string())
                .on_click_stop(move |_| {
                    cancel_data.close();
                })
                .style(move |s| {
                    let config = config.get();
                    s.padding_vert(5.0)
                        .padding_horiz(15.0)
                        .border(1.0)
                        .border_radius(LapceLayout::BORDER_RADIUS)
                        .border_color(config.color(LapceColor::LAPCE_BORDER))
                        .color(config.color(LapceColor::EDITOR_FOREGROUND))
                        .cursor(CursorStyle::Pointer)
                        .hover(|s| {
                            s.background(
                                config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                        })
                }),
        ))
        .style(|s| s.flex_row().gap(10.0).justify_end()),
    )
    .style(move |s| {
        let config = config.get();
        s.width_full()
            .padding_vert(10.0)
            .padding_horiz(15.0)
            .border_top(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
    })
}
