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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_bottom_true_for_bottom_positions() {
        assert!(PanelPosition::BottomLeft.is_bottom());
        assert!(PanelPosition::BottomRight.is_bottom());
    }

    #[test]
    fn is_bottom_false_for_non_bottom_positions() {
        assert!(!PanelPosition::LeftTop.is_bottom());
        assert!(!PanelPosition::LeftBottom.is_bottom());
        assert!(!PanelPosition::RightTop.is_bottom());
        assert!(!PanelPosition::RightBottom.is_bottom());
    }

    #[test]
    fn is_right_true_for_right_positions() {
        assert!(PanelPosition::RightTop.is_right());
        assert!(PanelPosition::RightBottom.is_right());
    }

    #[test]
    fn is_right_false_for_non_right_positions() {
        assert!(!PanelPosition::LeftTop.is_right());
        assert!(!PanelPosition::LeftBottom.is_right());
        assert!(!PanelPosition::BottomLeft.is_right());
        assert!(!PanelPosition::BottomRight.is_right());
    }

    #[test]
    fn is_left_true_for_left_positions() {
        assert!(PanelPosition::LeftTop.is_left());
        assert!(PanelPosition::LeftBottom.is_left());
    }

    #[test]
    fn is_left_false_for_non_left_positions() {
        assert!(!PanelPosition::RightTop.is_left());
        assert!(!PanelPosition::RightBottom.is_left());
        assert!(!PanelPosition::BottomLeft.is_left());
        assert!(!PanelPosition::BottomRight.is_left());
    }

    #[test]
    fn is_first_returns_true_for_first_halves() {
        assert!(PanelPosition::LeftTop.is_first());
        assert!(PanelPosition::BottomLeft.is_first());
        assert!(PanelPosition::RightTop.is_first());
    }

    #[test]
    fn is_first_returns_false_for_second_halves() {
        assert!(!PanelPosition::LeftBottom.is_first());
        assert!(!PanelPosition::BottomRight.is_first());
        assert!(!PanelPosition::RightBottom.is_first());
    }

    #[test]
    fn peer_returns_other_half_of_same_container() {
        assert_eq!(PanelPosition::LeftTop.peer(), PanelPosition::LeftBottom);
        assert_eq!(PanelPosition::LeftBottom.peer(), PanelPosition::LeftTop);
        assert_eq!(PanelPosition::BottomLeft.peer(), PanelPosition::BottomRight);
        assert_eq!(PanelPosition::BottomRight.peer(), PanelPosition::BottomLeft);
        assert_eq!(PanelPosition::RightTop.peer(), PanelPosition::RightBottom);
        assert_eq!(PanelPosition::RightBottom.peer(), PanelPosition::RightTop);
    }

    #[test]
    fn peer_is_involution() {
        let all = [
            PanelPosition::LeftTop,
            PanelPosition::LeftBottom,
            PanelPosition::BottomLeft,
            PanelPosition::BottomRight,
            PanelPosition::RightTop,
            PanelPosition::RightBottom,
        ];
        for pos in all {
            assert_eq!(pos.peer().peer(), pos, "peer(peer({pos:?})) != {pos:?}");
        }
    }

    #[test]
    fn container_is_bottom() {
        assert!(PanelContainerPosition::Bottom.is_bottom());
        assert!(!PanelContainerPosition::Left.is_bottom());
        assert!(!PanelContainerPosition::Right.is_bottom());
    }

    #[test]
    fn container_first_returns_correct_position() {
        assert_eq!(PanelContainerPosition::Left.first(), PanelPosition::LeftTop);
        assert_eq!(
            PanelContainerPosition::Bottom.first(),
            PanelPosition::BottomLeft
        );
        assert_eq!(
            PanelContainerPosition::Right.first(),
            PanelPosition::RightTop
        );
    }

    #[test]
    fn container_second_returns_correct_position() {
        assert_eq!(
            PanelContainerPosition::Left.second(),
            PanelPosition::LeftBottom
        );
        assert_eq!(
            PanelContainerPosition::Bottom.second(),
            PanelPosition::BottomRight
        );
        assert_eq!(
            PanelContainerPosition::Right.second(),
            PanelPosition::RightBottom
        );
    }

    #[test]
    fn first_peer_equals_second_for_each_container() {
        let containers = [
            PanelContainerPosition::Left,
            PanelContainerPosition::Bottom,
            PanelContainerPosition::Right,
        ];
        for container in containers {
            assert_eq!(
                container.first().peer(),
                container.second(),
                "first().peer() != second() for {container:?}"
            );
        }
    }
}
