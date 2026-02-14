use std::{ops::Range, path::PathBuf, rc::Rc};

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

impl SearchMatchData {
    /// The dynamic height used by the virtual_stack's `item_size_fn`. When collapsed,
    /// only the file header row is counted. When expanded, each match adds one row.
    pub fn height(&self) -> f64 {
        let line_height = self.line_height.get();
        let count = if self.expanded.get() {
            self.matches.with(|m| m.len()) + 1
        } else {
            1
        };
        line_height * count as f64
    }
}

/// The shared search backend used by both the search modal and the search panel.
/// Results are stored as an IndexMap<PathBuf, SearchMatchData> to maintain file
/// order from the proxy. The `editor` is the panel's input editor, while both
/// the modal and panel can call `set_pattern()` to programmatically trigger searches.
/// `preview_editor` is the panel's side-by-side preview; the modal has its own.
#[derive(Clone, Debug)]
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
            CommandKind::Workbench(_) => {}
            CommandKind::Scroll(_) => {}
            CommandKind::Focus(cmd) => match cmd {
                FocusCommand::ListNext => self.next(),
                FocusCommand::ListPrevious => self.previous(),
                FocusCommand::ListSelect => self.select(),
                _ => return CommandExecuted::No,
            },
            CommandKind::Edit(_)
            | CommandKind::Move(_)
            | CommandKind::MultiSelection(_) => {
                return self.editor.run_command(command, count, mods);
            }
            CommandKind::MotionMode(_) => {}
        }
        CommandExecuted::No
    }

    fn receive_char(&self, c: &str) {
        if self.preview_focused.get_untracked() {
            self.preview_editor.receive_char(c);
        } else {
            self.editor.receive_char(c);
        }
    }
}

/// VirtualVector impl for the hierarchical results view. total_len counts all
/// visible rows (file headers + expanded matches) for scroll height calculation.
/// Note: slice() ignores the range and returns ALL results because the outer
/// virtual_stack uses `item_size_fn` for variable-height items and needs the
/// full list. The inner virtual_stack for each file's matches handles its own
/// virtualization.
impl VirtualVector<(PathBuf, SearchMatchData)> for GlobalSearchData {
    fn total_len(&self) -> usize {
        self.search_result.with(|result| {
            result
                .iter()
                .map(|(_, data)| {
                    if data.expanded.get() {
                        data.matches.with(|m| m.len()) + 1
                    } else {
                        1
                    }
                })
                .sum()
        })
    }

    fn slice(
        &mut self,
        _range: Range<usize>,
    ) -> impl Iterator<Item = (PathBuf, SearchMatchData)> {
        self.search_result.get().into_iter()
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

        let global_search = Self {
            editor,
            search_result,
            main_split,
            common,
            preview_editor,
            has_preview,
            selected_match,
            preview_focused,
        };

        // Reactive effect: whenever the editor buffer text changes, fire a new search
        // request to the proxy. The proxy performs the actual file-system search in the
        // background and sends results back via create_ext_action. This is debounce-free:
        // every keystroke triggers a new search (the proxy handles cancellation of old ones).
        {
            let global_search = global_search.clone();
            let buffer = global_search.editor.doc().buffer;
            cx.create_effect(move |_| {
                let pattern = buffer.with(|buffer| buffer.to_string());
                if pattern.is_empty() {
                    global_search.search_result.update(|r| r.clear());
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

        // Auto-preview first match when results change
        {
            let global_search = global_search.clone();
            let search_result = global_search.search_result;
            cx.create_effect(move |_| {
                let results = search_result.get();
                global_search.selected_match.set(None);
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

        self.search_result.set(
            matches
                .into_iter()
                .map(|(path, matches)| {
                    let match_data =
                        current.get(&path).cloned().unwrap_or_else(|| {
                            SearchMatchData {
                                expanded: self.common.scope.create_rw_signal(true),
                                matches: self
                                    .common
                                    .scope
                                    .create_rw_signal(im::Vector::new()),
                                line_height: self.common.ui_line_height,
                            }
                        });

                    match_data.matches.set(matches.into());

                    (path, match_data)
                })
                .collect(),
        );
    }

    /// Flattens the hierarchical results into a linear list of only the matches
    /// that are currently visible (i.e., their parent file group is expanded).
    /// This is needed for keyboard navigation (next/previous) through the results
    /// because the navigation is linear even though the view is hierarchical.
    fn visible_matches(&self) -> Vec<(PathBuf, usize, usize, usize)> {
        self.search_result.with_untracked(|results| {
            let mut flat = Vec::new();
            for (path, data) in results.iter() {
                if data.expanded.get_untracked() {
                    data.matches.with_untracked(|matches| {
                        for m in matches.iter() {
                            flat.push((path.clone(), m.line, m.start, m.end));
                        }
                    });
                }
            }
            flat
        })
    }

    fn next(&self) {
        self.preview_focused.set(false);
        let flat = self.visible_matches();
        if flat.is_empty() {
            return;
        }
        let current = self.selected_match.get_untracked();
        let next_idx = match &current {
            Some(sel) => {
                let pos = flat.iter().position(|m| m == sel);
                match pos {
                    Some(i) if i + 1 < flat.len() => i + 1,
                    Some(_) => return,
                    None => 0,
                }
            }
            None => 0,
        };
        let next = flat[next_idx].clone();
        self.selected_match.set(Some(next.clone()));
        self.preview_match(next.0, next.1);
    }

    fn previous(&self) {
        self.preview_focused.set(false);
        let flat = self.visible_matches();
        if flat.is_empty() {
            return;
        }
        let current = self.selected_match.get_untracked();
        let prev_idx = match &current {
            Some(sel) => {
                let pos = flat.iter().position(|m| m == sel);
                match pos {
                    Some(i) if i > 0 => i - 1,
                    Some(_) => return,
                    None => 0,
                }
            }
            None => 0,
        };
        let prev = flat[prev_idx].clone();
        self.selected_match.set(Some(prev.clone()));
        self.preview_match(prev.0, prev.1);
    }

    fn select(&self) {
        if let Some((path, line, _, _)) = self.selected_match.get_untracked() {
            self.common
                .internal_command
                .send(InternalCommand::JumpToLocation {
                    location: EditorLocation {
                        path,
                        position: Some(EditorPosition::Line(line.saturating_sub(1))),
                        scroll_offset: None,
                        same_editor_tab: false,
                    },
                });
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
