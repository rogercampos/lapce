use crate::{
    folder_picker::FolderPickerData, go_to_file::GoToFileData,
    go_to_line::GoToLineData, go_to_symbol::GoToSymbolData,
    recent_files::RecentFilesData,
};

/// All palette-style popups surfaced from the workbench. Each is a self-contained
/// modal with its own input editor, filtered list, and keyboard focus routing;
/// grouping them here keeps `WorkspaceData` scoped to orchestration.
#[derive(Clone)]
pub struct Palettes {
    pub go_to_file: GoToFileData,
    pub go_to_line: GoToLineData,
    pub go_to_symbol: GoToSymbolData,
    pub recent_files: RecentFilesData,
    pub folder_picker: FolderPickerData,
}
