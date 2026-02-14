/// Type aliases for Floem's Id type, providing semantic names for different
/// kinds of identifiers. Using distinct types makes function signatures
/// self-documenting and prevents accidentally passing a SplitId where an
/// EditorTabId is expected (though they are structurally the same type).
use floem::views::editor::id::Id;

pub type SplitId = Id;
pub type WorkspaceId = Id;
pub type EditorTabId = Id;
pub type SettingsId = Id;
pub type KeymapId = Id;
pub type ThemeColorSettingsId = Id;
pub type VoltViewId = Id;
pub type DiffEditorId = Id;
pub type TerminalTabId = Id;
