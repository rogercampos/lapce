use std::rc::Rc;

use floem::{View, reactive::SignalGet, views::Decorators};

use super::position::PanelPosition;
use crate::{
    panel::implementation_view::common_reference_panel,
    workspace_data::WorkspaceData,
};

pub fn references_panel(
    workspace_data: Rc<WorkspaceData>,
    _position: PanelPosition,
) -> impl View {
    common_reference_panel(workspace_data.clone(), _position, move || {
        workspace_data.main_split.references.get()
    })
    .debug_name("references panel")
}
