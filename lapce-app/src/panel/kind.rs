use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

use super::position::PanelPosition;
use crate::config::icon::LapceIcons;

/// Each variant maps to a specific panel view. EnumIter is derived so that
/// we can iterate all panel kinds for building the panel picker icons.
#[derive(
    Clone, Copy, PartialEq, Serialize, Deserialize, Hash, Eq, Debug, EnumIter,
)]
pub enum PanelKind {
    FileExplorer,
    Search,
}

impl PanelKind {
    pub fn svg_name(&self) -> &'static str {
        match &self {
            PanelKind::FileExplorer => LapceIcons::FILE_EXPLORER,
            PanelKind::Search => LapceIcons::SEARCH,
        }
    }

    /// The position where a panel appears by default when no persisted state exists.
    /// This only matters for initial workspace setup; afterwards persisted state takes over.
    pub fn default_position(&self) -> PanelPosition {
        match self {
            PanelKind::FileExplorer => PanelPosition::LeftTop,
            PanelKind::Search => PanelPosition::BottomLeft,
        }
    }
}
