use crate::{
    global_search::GlobalSearchData, replace_modal::ReplaceModalData,
    search_modal::SearchModalData, search_tabs::SearchTabsData,
};

/// All workspace-level search entry points: the always-on backend
/// (`GlobalSearchData`), the in-workspace `search/replace` modal popups, and
/// the per-query tabs shown in the bottom panel. The modal popups hold their
/// own references to `GlobalSearchData` because they share the backend but
/// own independent UI state.
#[derive(Clone)]
pub struct SearchState {
    pub global: GlobalSearchData,
    pub tabs: SearchTabsData,
    pub modal: SearchModalData,
    pub replace_modal: ReplaceModalData,
}
