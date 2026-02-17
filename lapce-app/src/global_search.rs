use std::{
    collections::{BTreeMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use floem::{
    ext_event::create_ext_action,
    keyboard::Modifiers,
    reactive::{Memo, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    views::VirtualVector,
};
use indexmap::IndexMap;
use lapce_core::{command::FocusCommand, selection::Selection};
use lapce_rpc::proxy::{ProxyResponse, SearchMatch};
use lapce_xi_rope::Rope;

use crate::{
    command::{CommandExecuted, CommandKind, InternalCommand},
    editor::{
        EditorData, EditorViewKind,
        location::{EditorLocation, EditorPosition},
    },
    keypress::{KeyPressFocus, condition::Condition},
    main_split::MainSplitData,
    workspace::LapceWorkspace,
    workspace_data::CommonData,
};

/// Per-file group of search matches. The `expanded` signal controls whether
/// individual matches are visible under the file header in the hierarchical
/// results view. `line_height` is shared from CommonData so that height
/// calculations stay in sync with the UI.
#[derive(Clone)]
pub struct SearchMatchData {
    pub expanded: RwSignal<bool>,
    pub matches: RwSignal<im::Vector<SearchMatch>>,
    pub line_height: Memo<f64>,
}

/// A single row in the flattened search tree for the virtual stack.
#[derive(Clone, Debug, PartialEq)]
pub enum SearchTreeRow {
    Folder {
        rel_path: PathBuf,
        name: String,
        expanded: bool,
        match_count: usize,
        level: usize,
    },
    File {
        full_path: PathBuf,
        name: String,
        expanded: bool,
        match_count: usize,
        level: usize,
    },
    Match {
        full_path: PathBuf,
        search_match: SearchMatch,
        level: usize,
    },
}

impl SearchTreeRow {
    /// Unique key for virtual_stack identity.
    pub fn key(&self) -> String {
        match self {
            SearchTreeRow::Folder { rel_path, .. } => {
                format!("folder:{}", rel_path.display())
            }
            SearchTreeRow::File { full_path, .. } => {
                format!("file:{}", full_path.display())
            }
            SearchTreeRow::Match {
                full_path,
                search_match,
                ..
            } => {
                format!(
                    "match:{}:{}:{}:{}",
                    full_path.display(),
                    search_match.line,
                    search_match.start,
                    search_match.end
                )
            }
        }
    }
}

/// VirtualVector adapter for the flat search tree rows.
pub struct SearchTreeVirtualList(pub Vec<SearchTreeRow>);

impl VirtualVector<SearchTreeRow> for SearchTreeVirtualList {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(&mut self, range: Range<usize>) -> impl Iterator<Item = SearchTreeRow> {
        let start = range.start.min(self.0.len());
        let end = range.end.min(self.0.len());
        self.0[start..end].to_vec().into_iter()
    }
}

/// Internal tree node used during tree construction.
enum TreeEntry {
    Folder {
        name: String,
        children: BTreeMap<String, TreeEntry>,
    },
    File {
        name: String,
        full_path: PathBuf,
        match_count: usize,
        matches: Vec<SearchMatch>,
    },
}

impl TreeEntry {
    /// Recursively count all matches under this entry.
    fn total_match_count(&self) -> usize {
        match self {
            TreeEntry::File { match_count, .. } => *match_count,
            TreeEntry::Folder { children, .. } => {
                children.values().map(|c| c.total_match_count()).sum()
            }
        }
    }
}

/// The shared search backend used by both the search modal and the search panel.
/// Results are stored as an IndexMap<PathBuf, SearchMatchData> to maintain file
/// order from the proxy. The `editor` is the panel's input editor, while both
/// the modal and panel can call `set_pattern()` to programmatically trigger searches.
/// `preview_editor` is the panel's side-by-side preview; the modal has its own.
#[derive(Clone)]
pub struct GlobalSearchData {
    pub editor: EditorData,
    /// Hierarchical results: file path -> per-file match data. IndexMap preserves
    /// insertion order so results appear in the order the proxy returns them.
    pub search_result: RwSignal<IndexMap<PathBuf, SearchMatchData>>,
    pub main_split: MainSplitData,
    pub common: Rc<CommonData>,
    pub preview_editor: EditorData,
    pub has_preview: RwSignal<bool>,
    /// Currently selected match: (file_path, line, start_col, end_col).
    /// Used for highlighting and preview synchronization.
    pub selected_match: RwSignal<Option<(PathBuf, usize, usize, usize)>>,
    /// When true, keyboard input is forwarded to the preview editor instead of
    /// the result list navigation. Set on click into the preview, cleared on
    /// list navigation (next/previous).
    pub preview_focused: RwSignal<bool>,
    /// Set of collapsed folder paths (relative). Folders not in this set are expanded.
    pub collapsed_folders: RwSignal<HashSet<PathBuf>>,
    /// Set of collapsed file paths (absolute). Files not in this set are expanded.
    pub collapsed_files: RwSignal<HashSet<PathBuf>>,
    /// Flat list of visible rows derived from the folder tree. Used by the
    /// panel's virtual_stack for rendering.
    pub search_tree_rows: Memo<Vec<SearchTreeRow>>,
    /// Currently selected row index into search_tree_rows for keyboard navigation.
    pub selected_index: RwSignal<Option<usize>>,
    /// Workspace reference for stripping path prefixes.
    pub workspace: Arc<LapceWorkspace>,
    /// Separate result set for the bottom panel. Only updated when:
    /// - Searching directly from the panel (modal not active), or
    /// - "Open full results" is clicked from the modal.
    pub panel_search_result: RwSignal<IndexMap<PathBuf, SearchMatchData>>,
    /// True while the search modal is open. Prevents live search results from
    /// propagating to the panel.
    pub modal_active: RwSignal<bool>,
}

impl std::fmt::Debug for GlobalSearchData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalSearchData").finish()
    }
}

impl KeyPressFocus for GlobalSearchData {
    /// The preview_focused pattern: when the preview is focused we report
    /// EditorFocus (not ListFocus) so arrow keys work as editor movement
    /// rather than list navigation. PanelFocus is always reported so
    /// panel-specific keybindings (like toggle maximize) still fire.
    fn check_condition(&self, condition: Condition) -> bool {
        if self.preview_focused.get_untracked() {
            matches!(condition, Condition::PanelFocus | Condition::EditorFocus)
        } else {
            matches!(condition, Condition::PanelFocus | Condition::ListFocus)
        }
    }

    fn run_command(
        &self,
        command: &crate::command::LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        if self.preview_focused.get_untracked() {
            return self.preview_editor.run_command(command, count, mods);
        }
        match &command.kind {
            CommandKind::Focus(cmd) => match cmd {
                FocusCommand::ListNext => {
                    self.next();
                    CommandExecuted::Yes
                }
                FocusCommand::ListPrevious => {
                    self.previous();
                    CommandExecuted::Yes
                }
                FocusCommand::ListSelect => {
                    self.select();
                    CommandExecuted::Yes
                }
                _ => CommandExecuted::No,
            },
            CommandKind::Edit(_)
            | CommandKind::Move(_)
            | CommandKind::MultiSelection(_) => {
                self.editor.run_command(command, count, mods)
            }
            _ => CommandExecuted::No,
        }
    }

    fn receive_char(&self, c: &str) {
        if self.preview_focused.get_untracked() {
            self.preview_editor.receive_char(c);
        } else {
            self.editor.receive_char(c);
        }
    }
}

impl GlobalSearchData {
    pub fn new(cx: Scope, main_split: MainSplitData) -> Self {
        let common = main_split.common.clone();
        let editor = main_split.editors.make_local(cx, common.clone());
        let search_result = cx.create_rw_signal(IndexMap::new());
        let preview_editor = main_split.editors.make_local(cx, common.clone());
        preview_editor.kind.set(EditorViewKind::Preview);
        let has_preview = cx.create_rw_signal(false);
        let selected_match = cx.create_rw_signal(None);
        let preview_focused = cx.create_rw_signal(false);
        let collapsed_folders: RwSignal<HashSet<PathBuf>> =
            cx.create_rw_signal(HashSet::new());
        let collapsed_files: RwSignal<HashSet<PathBuf>> =
            cx.create_rw_signal(HashSet::new());
        let selected_index = cx.create_rw_signal(None);
        let workspace = common.workspace.clone();
        let panel_search_result = cx.create_rw_signal(IndexMap::new());
        let modal_active = cx.create_rw_signal(false);

        // Build the search_tree_rows Memo from panel_search_result (not
        // search_result) so the bottom panel only updates when explicitly
        // committed, not during live modal typing.
        let search_tree_rows = {
            let workspace = workspace.clone();
            cx.create_memo(move |_| {
                let results = panel_search_result.get();
                if results.is_empty() {
                    return Vec::new();
                }

                let collapsed_f = collapsed_folders.get();
                let collapsed_fi = collapsed_files.get();

                let workspace_path = workspace.path.as_deref();
                let tree = build_search_tree(&results, workspace_path);

                let mut rows = Vec::new();
                flatten_tree_entries(
                    &tree,
                    &collapsed_f,
                    &collapsed_fi,
                    0,
                    &mut rows,
                    &PathBuf::new(),
                );
                rows
            })
        };

        let global_search = Self {
            editor,
            search_result,
            main_split,
            common,
            preview_editor,
            has_preview,
            selected_match,
            preview_focused,
            collapsed_folders,
            collapsed_files,
            search_tree_rows,
            selected_index,
            workspace,
            panel_search_result,
            modal_active,
        };

        // Reactive effect: whenever the editor buffer text changes, fire a new search
        // request to the proxy.
        {
            let global_search = global_search.clone();
            let buffer = global_search.editor.doc().buffer;
            cx.create_effect(move |_| {
                let pattern = buffer.with(|buffer| buffer.to_string());
                if pattern.is_empty() {
                    global_search.search_result.update(|r| r.clear());
                    if !global_search.modal_active.get_untracked() {
                        global_search.panel_search_result.update(|r| r.clear());
                    }
                    return;
                }
                let case_sensitive = global_search.common.find.case_sensitive(true);
                let whole_word = global_search.common.find.whole_words.get();
                let is_regex = global_search.common.find.is_regex.get();
                let send = {
                    let global_search = global_search.clone();
                    create_ext_action(cx, move |result| {
                        if let Ok(ProxyResponse::GlobalSearchResponse { matches }) =
                            result
                        {
                            global_search.update_matches(matches);
                        }
                    })
                };
                global_search.common.proxy.global_search(
                    pattern,
                    case_sensitive,
                    whole_word,
                    is_regex,
                    move |result| {
                        send(result);
                    },
                );
            });
        }

        // Auto-preview first match when panel results change
        {
            let global_search = global_search.clone();
            let panel_search_result = global_search.panel_search_result;
            cx.create_effect(move |_| {
                let results = panel_search_result.get();
                global_search.selected_match.set(None);
                global_search.selected_index.set(None);
                if let Some((path, match_data)) = results.iter().next() {
                    if let Some(first) =
                        match_data.matches.get_untracked().iter().next().cloned()
                    {
                        global_search.selected_match.set(Some((
                            path.clone(),
                            first.line,
                            first.start,
                            first.end,
                        )));
                        global_search.preview_match(path.clone(), first.line);
                        // Find the index of the first match row
                        let rows = global_search.search_tree_rows.get_untracked();
                        for (i, row) in rows.iter().enumerate() {
                            if let SearchTreeRow::Match {
                                full_path,
                                search_match,
                                ..
                            } = row
                            {
                                if full_path == path
                                    && search_match.line == first.line
                                    && search_match.start == first.start
                                    && search_match.end == first.end
                                {
                                    global_search.selected_index.set(Some(i));
                                    break;
                                }
                            }
                        }
                    } else {
                        global_search.has_preview.set(false);
                    }
                } else {
                    global_search.has_preview.set(false);
                }
            });
        }

        global_search
    }

    /// Merges new proxy results into the existing result map. Reuses existing
    /// SearchMatchData (and its RwSignals) for files that were already present,
    /// preserving their expanded/collapsed state across search updates.
    fn update_matches(&self, matches: IndexMap<PathBuf, Vec<SearchMatch>>) {
        let current = self.search_result.get_untracked();

        let new_results: IndexMap<PathBuf, SearchMatchData> = matches
            .into_iter()
            .map(|(path, matches)| {
                let match_data =
                    current
                        .get(&path)
                        .cloned()
                        .unwrap_or_else(|| SearchMatchData {
                            expanded: self.common.scope.create_rw_signal(true),
                            matches: self
                                .common
                                .scope
                                .create_rw_signal(im::Vector::new()),
                            line_height: self.common.ui_line_height,
                        });

                match_data.matches.set(matches.into());

                (path, match_data)
            })
            .collect();

        self.search_result.set(new_results.clone());

        // Only propagate to the panel when the modal is not driving the search
        if !self.modal_active.get_untracked() {
            self.panel_search_result.set(new_results);
        }
    }

    /// Copy the current search results to the panel's result set.
    /// Called when "Open full results" is clicked from the modal.
    pub fn commit_results_to_panel(&self) {
        let results = self.search_result.get_untracked();
        self.panel_search_result.set(results);
    }

    /// Toggle expanded state for a folder path.
    pub fn toggle_folder(&self, rel_path: &Path) {
        self.collapsed_folders.update(|set| {
            if !set.remove(rel_path) {
                set.insert(rel_path.to_path_buf());
            }
        });
    }

    /// Toggle expanded state for a file path.
    pub fn toggle_file(&self, full_path: &Path) {
        self.collapsed_files.update(|set| {
            if !set.remove(full_path) {
                set.insert(full_path.to_path_buf());
            }
        });
    }

    fn next(&self) {
        self.preview_focused.set(false);
        let rows = self.search_tree_rows.get_untracked();
        if rows.is_empty() {
            return;
        }
        let current = self.selected_index.get_untracked();
        let next_idx = match current {
            Some(i) if i + 1 < rows.len() => i + 1,
            Some(_) => return,
            None => 0,
        };
        self.selected_index.set(Some(next_idx));
        self.update_selection_from_row(&rows[next_idx]);
    }

    fn previous(&self) {
        self.preview_focused.set(false);
        let rows = self.search_tree_rows.get_untracked();
        if rows.is_empty() {
            return;
        }
        let current = self.selected_index.get_untracked();
        let prev_idx = match current {
            Some(i) if i > 0 => i - 1,
            Some(_) => return,
            None => 0,
        };
        self.selected_index.set(Some(prev_idx));
        self.update_selection_from_row(&rows[prev_idx]);
    }

    fn select(&self) {
        let rows = self.search_tree_rows.get_untracked();
        if let Some(idx) = self.selected_index.get_untracked() {
            if let Some(row) = rows.get(idx) {
                match row {
                    SearchTreeRow::Folder { rel_path, .. } => {
                        self.toggle_folder(rel_path);
                    }
                    SearchTreeRow::File { full_path, .. } => {
                        self.toggle_file(full_path);
                    }
                    SearchTreeRow::Match {
                        full_path,
                        search_match,
                        ..
                    } => {
                        self.common.internal_command.send(
                            InternalCommand::JumpToLocation {
                                location: EditorLocation {
                                    path: full_path.clone(),
                                    position: Some(EditorPosition::Line(
                                        search_match.line.saturating_sub(1),
                                    )),
                                    scroll_offset: None,
                                    same_editor_tab: false,
                                },
                            },
                        );
                    }
                }
            }
        }
    }

    /// Update selected_match and preview based on the given row.
    fn update_selection_from_row(&self, row: &SearchTreeRow) {
        match row {
            SearchTreeRow::Match {
                full_path,
                search_match,
                ..
            } => {
                self.selected_match.set(Some((
                    full_path.clone(),
                    search_match.line,
                    search_match.start,
                    search_match.end,
                )));
                self.preview_match(full_path.clone(), search_match.line);
            }
            SearchTreeRow::File { full_path, .. } => {
                // Preview the first match in this file
                self.search_result.with_untracked(|results| {
                    if let Some(data) = results.get(full_path) {
                        if let Some(first) =
                            data.matches.get_untracked().iter().next().cloned()
                        {
                            self.selected_match.set(Some((
                                full_path.clone(),
                                first.line,
                                first.start,
                                first.end,
                            )));
                            self.preview_match(full_path.clone(), first.line);
                        }
                    }
                });
            }
            SearchTreeRow::Folder { .. } => {
                // No preview change for folder rows
            }
        }
    }

    pub fn preview_match(&self, path: PathBuf, line: usize) {
        let (doc, new_doc) = self.main_split.get_doc(path.clone(), None);
        self.preview_editor.update_doc(doc);
        self.preview_editor.go_to_location(
            EditorLocation {
                path,
                position: Some(EditorPosition::Line(line.saturating_sub(1))),
                scroll_offset: None,
                same_editor_tab: false,
            },
            new_doc,
            None,
        );
        self.has_preview.set(true);
    }

    pub fn set_pattern(&self, pattern: String) {
        let pattern_len = pattern.len();
        self.editor.doc().reload(Rope::from(pattern), true);
        self.editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::region(0, pattern_len)));
    }
}

/// Build a tree structure from flat search results, grouping files by directory.
fn build_search_tree(
    results: &IndexMap<PathBuf, SearchMatchData>,
    workspace_path: Option<&Path>,
) -> BTreeMap<String, TreeEntry> {
    let mut root: BTreeMap<String, TreeEntry> = BTreeMap::new();

    for (abs_path, match_data) in results.iter() {
        let rel_path = if let Some(wp) = workspace_path {
            abs_path.strip_prefix(wp).unwrap_or(abs_path)
        } else {
            abs_path.as_path()
        };

        let components: Vec<&str> = rel_path
            .parent()
            .map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_str().unwrap_or(""))
                    .collect()
            })
            .unwrap_or_default();

        let file_name = rel_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let matches_vec: Vec<SearchMatch> =
            match_data.matches.get_untracked().iter().cloned().collect();
        let match_count = matches_vec.len();

        // Navigate to the correct folder in the tree
        let mut current = &mut root;
        for component in &components {
            if component.is_empty() {
                continue;
            }
            let entry = current.entry(component.to_string()).or_insert_with(|| {
                TreeEntry::Folder {
                    name: component.to_string(),
                    children: BTreeMap::new(),
                }
            });
            current = match entry {
                TreeEntry::Folder { children, .. } => children,
                _ => unreachable!(),
            };
        }

        // Insert the file
        current.insert(
            file_name.clone(),
            TreeEntry::File {
                name: file_name,
                full_path: abs_path.clone(),
                match_count,
                matches: matches_vec,
            },
        );
    }

    root
}

/// Sort tree entries: directories first, then files, with human-sort on names.
fn sorted_keys(entries: &BTreeMap<String, TreeEntry>) -> Vec<String> {
    let mut keys: Vec<String> = entries.keys().cloned().collect();
    keys.sort_by(|a, b| {
        let a_is_dir = matches!(entries.get(a), Some(TreeEntry::Folder { .. }));
        let b_is_dir = matches!(entries.get(b), Some(TreeEntry::Folder { .. }));
        match (a_is_dir, b_is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => human_sort::compare(&a.to_lowercase(), &b.to_lowercase()),
        }
    });
    keys
}

/// Flatten the tree into a Vec<SearchTreeRow>, respecting expanded state.
/// `parent_rel` accumulates the relative path for folder uniqueness.
fn flatten_tree_entries(
    entries: &BTreeMap<String, TreeEntry>,
    collapsed_folders: &HashSet<PathBuf>,
    collapsed_files: &HashSet<PathBuf>,
    level: usize,
    rows: &mut Vec<SearchTreeRow>,
    parent_rel: &Path,
) {
    let keys = sorted_keys(entries);

    for key in keys {
        let entry = &entries[&key];
        match entry {
            TreeEntry::Folder { name, children } => {
                let rel_path = parent_rel.join(name);
                let expanded = !collapsed_folders.contains(&rel_path);

                rows.push(SearchTreeRow::Folder {
                    rel_path: rel_path.clone(),
                    name: name.clone(),
                    expanded,
                    match_count: entry.total_match_count(),
                    level,
                });

                if expanded {
                    flatten_tree_entries(
                        children,
                        collapsed_folders,
                        collapsed_files,
                        level + 1,
                        rows,
                        &rel_path,
                    );
                }
            }
            TreeEntry::File {
                name,
                full_path,
                match_count,
                matches,
            } => {
                let expanded = !collapsed_files.contains(full_path);

                rows.push(SearchTreeRow::File {
                    full_path: full_path.clone(),
                    name: name.clone(),
                    expanded,
                    match_count: *match_count,
                    level,
                });

                if expanded {
                    for m in matches {
                        rows.push(SearchTreeRow::Match {
                            full_path: full_path.clone(),
                            search_match: m.clone(),
                            level: level + 1,
                        });
                    }
                }
            }
        }
    }
}
