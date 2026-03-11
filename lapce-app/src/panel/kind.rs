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
    Schema,
}

impl PanelKind {
    pub fn svg_name(&self) -> &'static str {
        match &self {
            PanelKind::FileExplorer => LapceIcons::FILE_EXPLORER,
            PanelKind::Search => LapceIcons::SEARCH,
            PanelKind::Schema => LapceIcons::SYMBOL_KIND_FIELD,
        }
    }

    /// The position where a panel appears by default when no persisted state exists.
    /// This only matters for initial workspace setup; afterwards persisted state takes over.
    pub fn default_position(&self) -> PanelPosition {
        match self {
            PanelKind::FileExplorer => PanelPosition::LeftTop,
            PanelKind::Search => PanelPosition::BottomLeft,
            PanelKind::Schema => PanelPosition::RightTop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_name_file_explorer() {
        assert_eq!(
            PanelKind::FileExplorer.svg_name(),
            LapceIcons::FILE_EXPLORER
        );
    }

    #[test]
    fn svg_name_search() {
        assert_eq!(PanelKind::Search.svg_name(), LapceIcons::SEARCH);
    }

    #[test]
    fn default_position_file_explorer() {
        assert_eq!(
            PanelKind::FileExplorer.default_position(),
            PanelPosition::LeftTop
        );
    }

    #[test]
    fn default_position_search() {
        assert_eq!(
            PanelKind::Search.default_position(),
            PanelPosition::BottomLeft
        );
    }

    #[test]
    fn svg_name_schema() {
        assert_eq!(PanelKind::Schema.svg_name(), LapceIcons::SYMBOL_KIND_FIELD);
    }

    #[test]
    fn default_position_schema() {
        assert_eq!(
            PanelKind::Schema.default_position(),
            PanelPosition::RightTop
        );
    }
}
