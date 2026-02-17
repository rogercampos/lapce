use std::rc::Rc;

use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};
use lapce_xi_rope::find::CaseMatching;
use serde::{Deserialize, Serialize};

use crate::{
    global_search::GlobalSearchData, main_split::MainSplitData,
    workspace_data::CommonData,
};

/// Serializable info for a single search tab (pattern + options).
/// Results are NOT persisted; they are re-computed from the pattern on restore.
#[derive(Clone, Serialize, Deserialize)]
pub struct SearchTabInfo {
    pub pattern: String,
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub is_regex: bool,
}

/// Manages multiple search result tabs in the bottom panel.
/// Each tab is a full `GlobalSearchData` instance with its own results,
/// preview editor, and navigation state.
#[derive(Clone)]
pub struct SearchTabsData {
    pub tabs: RwSignal<im::Vector<GlobalSearchData>>,
    pub active_tab: RwSignal<usize>,
    pub main_split: MainSplitData,
    pub common: Rc<CommonData>,
    pub scope: Scope,
}

impl std::fmt::Debug for SearchTabsData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchTabsData").finish()
    }
}

impl SearchTabsData {
    pub fn new(
        cx: Scope,
        main_split: MainSplitData,
        common: Rc<CommonData>,
    ) -> Self {
        Self {
            tabs: cx.create_rw_signal(im::Vector::new()),
            active_tab: cx.create_rw_signal(0),
            main_split,
            common,
            scope: cx,
        }
    }

    /// Create a new search tab with the given pattern and options.
    /// Sets it as the active tab and triggers a search.
    pub fn new_tab(
        &self,
        pattern: String,
        case_matching: CaseMatching,
        whole_words: bool,
        is_regex: bool,
    ) {
        let gs = GlobalSearchData::new_for_tab(
            self.scope,
            self.main_split.clone(),
            pattern,
            case_matching,
            whole_words,
            is_regex,
        );
        let new_index = self.tabs.with_untracked(|tabs| tabs.len());
        self.tabs.update(|tabs| {
            tabs.push_back(gs);
        });
        self.active_tab.set(new_index);
    }

    /// Close a specific tab by index.
    pub fn close_tab(&self, index: usize) {
        let len = self.tabs.with_untracked(|tabs| tabs.len());
        if index >= len {
            return;
        }
        self.tabs.update(|tabs| {
            tabs.remove(index);
        });
        let new_len = len - 1;
        if new_len == 0 {
            self.active_tab.set(0);
        } else {
            let active = self.active_tab.get_untracked();
            if active >= new_len {
                self.active_tab.set(new_len - 1);
            } else if active > index {
                self.active_tab.set(active - 1);
            }
        }
    }

    /// Close all search tabs.
    pub fn close_all_tabs(&self) {
        self.tabs.update(|tabs| tabs.clear());
        self.active_tab.set(0);
    }

    /// Get the currently active tab's GlobalSearchData, if any.
    pub fn active_search(&self) -> Option<GlobalSearchData> {
        let active = self.active_tab.get_untracked();
        self.tabs.with_untracked(|tabs| tabs.get(active).cloned())
    }

    /// Set the active tab and trigger re-evaluation of its search.
    pub fn activate_tab(&self, index: usize) {
        let len = self.tabs.with_untracked(|tabs| tabs.len());
        if index >= len {
            return;
        }
        self.active_tab.set(index);
        if let Some(gs) = self.tabs.with_untracked(|tabs| tabs.get(index).cloned()) {
            gs.re_evaluate();
        }
    }

    /// Restore tabs from persisted info. Creates new GlobalSearchData for each.
    pub fn restore_from_info(&self, infos: Vec<SearchTabInfo>, active: usize) {
        for info in infos {
            let case_matching = if info.case_sensitive {
                CaseMatching::Exact
            } else {
                CaseMatching::CaseInsensitive
            };
            let gs = GlobalSearchData::new_for_tab(
                self.scope,
                self.main_split.clone(),
                info.pattern,
                case_matching,
                info.whole_words,
                info.is_regex,
            );
            self.tabs.update(|tabs| {
                tabs.push_back(gs);
            });
        }
        let len = self.tabs.with_untracked(|tabs| tabs.len());
        if len > 0 {
            self.active_tab.set(active.min(len - 1));
        }
    }

    /// Serialize all tabs to SearchTabInfo for persistence.
    pub fn tab_infos(&self) -> Vec<SearchTabInfo> {
        self.tabs.with_untracked(|tabs| {
            tabs.iter()
                .map(|gs| SearchTabInfo {
                    pattern: gs.pattern_text(),
                    case_sensitive: matches!(
                        gs.case_matching.get_untracked(),
                        CaseMatching::Exact
                    ),
                    whole_words: gs.whole_words.get_untracked(),
                    is_regex: gs.is_regex.get_untracked(),
                })
                .collect()
        })
    }

    /// Check if there are any tabs.
    pub fn has_tabs(&self) -> bool {
        self.tabs.with_untracked(|tabs| !tabs.is_empty())
    }
}
