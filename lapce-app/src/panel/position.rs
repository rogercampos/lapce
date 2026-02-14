use serde::{Deserialize, Serialize};

/// A specific slot within a panel container. Each container (Left, Bottom, Right)
/// is split into two halves (Top/Bottom or Left/Right), allowing two panel groups
/// per container. The "first" half is Top for vertical containers and Left for the
/// bottom horizontal container.
#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, Serialize, Deserialize)]
pub enum PanelPosition {
    LeftTop,
    LeftBottom,
    BottomLeft,
    BottomRight,
    RightTop,
    RightBottom,
}

impl PanelPosition {
    pub fn is_bottom(&self) -> bool {
        matches!(self, PanelPosition::BottomLeft | PanelPosition::BottomRight)
    }

    pub fn is_right(&self) -> bool {
        matches!(self, PanelPosition::RightTop | PanelPosition::RightBottom)
    }

    pub fn is_left(&self) -> bool {
        matches!(self, PanelPosition::LeftTop | PanelPosition::LeftBottom)
    }

    /// Whether this is the "first" (top or left) half of its container.
    /// Used to determine which side of the container the panel picker border appears on.
    pub fn is_first(&self) -> bool {
        matches!(
            self,
            PanelPosition::LeftTop
                | PanelPosition::BottomLeft
                | PanelPosition::RightTop
        )
    }

    /// Returns the other half of the same container (e.g. LeftTop <-> LeftBottom).
    /// Used when hiding a panel to also hide its peer if the peer slot is empty.
    pub fn peer(&self) -> PanelPosition {
        match &self {
            PanelPosition::LeftTop => PanelPosition::LeftBottom,
            PanelPosition::LeftBottom => PanelPosition::LeftTop,
            PanelPosition::BottomLeft => PanelPosition::BottomRight,
            PanelPosition::BottomRight => PanelPosition::BottomLeft,
            PanelPosition::RightTop => PanelPosition::RightBottom,
            PanelPosition::RightBottom => PanelPosition::RightTop,
        }
    }
}

/// The three physical panel containers in the layout. Each maps to two
/// PanelPosition halves. The container is the unit of visibility: if either
/// half is shown, the entire container is visible.
#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug)]
pub enum PanelContainerPosition {
    Left,
    Bottom,
    Right,
}

impl PanelContainerPosition {
    pub fn is_bottom(&self) -> bool {
        matches!(self, PanelContainerPosition::Bottom)
    }

    pub fn first(&self) -> PanelPosition {
        match self {
            PanelContainerPosition::Left => PanelPosition::LeftTop,
            PanelContainerPosition::Bottom => PanelPosition::BottomLeft,
            PanelContainerPosition::Right => PanelPosition::RightTop,
        }
    }

    pub fn second(&self) -> PanelPosition {
        match self {
            PanelContainerPosition::Left => PanelPosition::LeftBottom,
            PanelContainerPosition::Bottom => PanelPosition::BottomRight,
            PanelContainerPosition::Right => PanelPosition::RightBottom,
        }
    }
}
