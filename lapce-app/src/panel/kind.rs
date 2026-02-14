use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

use super::position::PanelPosition;
use crate::config::icon::LapceIcons;

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

    pub fn default_position(&self) -> PanelPosition {
        match self {
            PanelKind::FileExplorer => PanelPosition::LeftTop,
            PanelKind::Search => PanelPosition::BottomLeft,
        }
    }
}
