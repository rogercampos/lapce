use floem::{ViewId, reactive::RwSignal};

use crate::{
    about::AboutData, alert::AlertBoxData, code_action::CodeActionData,
    definition_picker::DefinitionPickerData, rename::RenameData,
};

/// Modal overlays rendered above the editor canvas. Each has its own focus
/// state and `KeyPressFocus` impl; grouping them here keeps `WorkspaceData`
/// focused on orchestration.
#[derive(Clone)]
pub struct Popups {
    pub code_action: RwSignal<CodeActionData>,
    pub definition_picker: RwSignal<DefinitionPickerData>,
    /// Currently-rendered code-lens popup view. `None` when not shown.
    pub code_lens: RwSignal<Option<ViewId>>,
    pub rename: RenameData,
    pub alert: AlertBoxData,
    pub about: AboutData,
}
