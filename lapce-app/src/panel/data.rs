use std::rc::Rc;

use floem::{
    kurbo::Size,
    reactive::{Memo, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
};
use serde::{Deserialize, Serialize};

use super::{
    kind::PanelKind,
    position::{PanelContainerPosition, PanelPosition},
    style::PanelStyle,
};
use crate::workspace_data::{CommonData, Focus};

/// Maps each panel position slot to the ordered list of panel kinds it contains.
/// im::HashMap is used for cheap cloning since this is stored in a reactive signal.
pub type PanelOrder = im::HashMap<PanelPosition, im::Vector<PanelKind>>;

/// Defines the fixed panel layout. This is the source of truth for which panels
/// exist in which positions. There is no runtime reordering.
pub fn default_panel_order() -> PanelOrder {
    let mut order = PanelOrder::new();
    order.insert(PanelPosition::LeftTop, im::vector![PanelKind::FileExplorer]);
    order.insert(PanelPosition::BottomLeft, im::vector![PanelKind::Search]);

    order
}

/// Foldable sections within panels (e.g. "Open Editors" and "File Explorer" within
/// the file explorer panel). Each section's fold state (open/closed) is persisted
/// per workspace so that the user's preferred layout is remembered.
#[derive(Clone, Copy, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub enum PanelSection {
    OpenEditor,
    FileExplorer,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PanelSize {
    pub left: f64,
    pub left_split: f64,
    pub bottom: f64,
    pub bottom_split: f64,
    pub right: f64,
    pub right_split: f64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PanelInfo {
    pub styles: im::HashMap<PanelPosition, PanelStyle>,
    pub size: PanelSize,
    pub sections: im::HashMap<PanelSection, bool>,
}

/// Central state for the entire panel system. `panels` holds the fixed layout
/// order, `styles` holds per-position visibility/active tab/maximized state,
/// `size` holds the drag-resizable dimensions, and `sections` holds fold states.
#[derive(Clone)]
pub struct PanelData {
    pub panels: RwSignal<PanelOrder>,
    pub styles: RwSignal<im::HashMap<PanelPosition, PanelStyle>>,
    pub size: RwSignal<PanelSize>,
    pub available_size: Memo<Size>,
    /// Each section's open/closed signal is stored individually so that toggling
    /// one section does not trigger re-renders for others.
    pub sections: RwSignal<im::HashMap<PanelSection, RwSignal<bool>>>,
    pub common: Rc<CommonData>,
}

impl PanelData {
    pub fn new(
        cx: Scope,
        available_size: Memo<Size>,
        sections: im::HashMap<PanelSection, bool>,
        common: Rc<CommonData>,
    ) -> Self {
        let panels = cx.create_rw_signal(default_panel_order());

        let mut styles = im::HashMap::new();
        styles.insert(
            PanelPosition::LeftTop,
            PanelStyle {
                active: 0,
                shown: true,
                maximized: false,
            },
        );
        styles.insert(
            PanelPosition::LeftBottom,
            PanelStyle {
                active: 0,
                shown: false,
                maximized: false,
            },
        );
        styles.insert(
            PanelPosition::BottomLeft,
            PanelStyle {
                active: 0,
                shown: false,
                maximized: false,
            },
        );
        styles.insert(
            PanelPosition::BottomRight,
            PanelStyle {
                active: 0,
                shown: false,
                maximized: false,
            },
        );
        styles.insert(
            PanelPosition::RightTop,
            PanelStyle {
                active: 0,
                shown: false,
                maximized: false,
            },
        );
        styles.insert(
            PanelPosition::RightBottom,
            PanelStyle {
                active: 0,
                shown: false,
                maximized: false,
            },
        );
        let styles = cx.create_rw_signal(styles);
        let size = cx.create_rw_signal(PanelSize {
            left: 250.0,
            left_split: 0.5,
            bottom: 300.0,
            bottom_split: 0.5,
            right: 250.0,
            right_split: 0.5,
        });
        let sections = cx.create_rw_signal(
            sections
                .into_iter()
                .map(|(key, value)| (key, cx.create_rw_signal(value)))
                .collect(),
        );

        Self {
            panels,
            styles,
            size,
            available_size,
            sections,
            common,
        }
    }

    pub fn panel_info(&self) -> PanelInfo {
        PanelInfo {
            styles: self.styles.get_untracked(),
            size: self.size.get_untracked(),
            sections: self
                .sections
                .get_untracked()
                .into_iter()
                .map(|(key, value)| (key, value.get_untracked()))
                .collect(),
        }
    }

    /// Returns whether a panel container (e.g., Left, Bottom, Right) has any
    /// visible panel position.
    ///
    /// The `tracked` parameter controls signal subscription behavior:
    /// - `tracked=true`: Use in view/style closures so the UI re-renders when
    ///   visibility changes (calls `signal.get()` which subscribes to updates).
    /// - `tracked=false`: Use in command handlers and initialization code where
    ///   re-rendering is not desired (calls `signal.get_untracked()`).
    pub fn is_container_shown(
        &self,
        position: &PanelContainerPosition,
        tracked: bool,
    ) -> bool {
        self.is_position_shown(&position.first(), tracked)
            || self.is_position_shown(&position.second(), tracked)
    }

    /// Returns whether a panel position slot has no panels assigned.
    ///
    /// `tracked=true` for view/style closures, `tracked=false` for command handlers.
    /// See `is_container_shown()` for details on the tracking pattern.
    pub fn is_position_empty(
        &self,
        position: &PanelPosition,
        tracked: bool,
    ) -> bool {
        if tracked {
            self.panels
                .with(|panels| panels.get(position).map(|p| p.is_empty()))
                .unwrap_or(true)
        } else {
            self.panels
                .with_untracked(|panels| panels.get(position).map(|p| p.is_empty()))
                .unwrap_or(true)
        }
    }

    /// Returns whether a panel position is currently shown (visible).
    ///
    /// `tracked=true` for view/style closures, `tracked=false` for command handlers.
    /// See `is_container_shown()` for details on the tracking pattern.
    pub fn is_position_shown(
        &self,
        position: &PanelPosition,
        tracked: bool,
    ) -> bool {
        let styles = if tracked {
            self.styles.get()
        } else {
            self.styles.get_untracked()
        };
        styles.get(position).map(|s| s.shown).unwrap_or(false)
    }

    pub fn panel_position(
        &self,
        kind: &PanelKind,
    ) -> Option<(usize, PanelPosition)> {
        self.panels
            .with_untracked(|panels| panel_position(panels, kind))
    }

    pub fn is_panel_visible(&self, kind: &PanelKind) -> bool {
        if let Some((index, position)) = self.panel_position(kind) {
            if let Some(style) = self
                .styles
                .with_untracked(|styles| styles.get(&position).cloned())
            {
                return style.active == index && style.shown;
            }
        }
        false
    }

    pub fn show_panel(&self, kind: &PanelKind) {
        if let Some((index, position)) = self.panel_position(kind) {
            self.styles.update(|styles| {
                if let Some(style) = styles.get_mut(&position) {
                    style.shown = true;
                    style.active = index;
                }
            });
        }
    }

    /// Hides the panel, but only if it is currently the active panel at its position.
    /// Also hides the peer position if it has no panels, preventing an empty container
    /// from being displayed (e.g. hiding LeftTop also hides LeftBottom if it is empty).
    pub fn hide_panel(&self, kind: &PanelKind) {
        if let Some((_, position)) = self.panel_position(kind) {
            if let Some((active_panel, _)) =
                self.active_panel_at_position(&position, false)
            {
                if &active_panel == kind {
                    self.set_shown(&position, false);
                    let peer_position = position.peer();
                    if let Some(order) = self
                        .panels
                        .with_untracked(|panels| panels.get(&peer_position).cloned())
                    {
                        if order.is_empty() {
                            self.set_shown(&peer_position, false);
                        }
                    }
                }
            }
        }
    }

    /// Get the active panel kind at that position, if any.
    ///
    /// `tracked=true` for view/style closures, `tracked=false` for command handlers.
    /// See `is_container_shown()` for details on the tracking pattern.
    pub fn active_panel_at_position(
        &self,
        position: &PanelPosition,
        tracked: bool,
    ) -> Option<(PanelKind, bool)> {
        let style = if tracked {
            self.styles.with(|styles| styles.get(position).cloned())?
        } else {
            self.styles
                .with_untracked(|styles| styles.get(position).cloned())?
        };
        let order = if tracked {
            self.panels.with(|panels| panels.get(position).cloned())?
        } else {
            self.panels
                .with_untracked(|panels| panels.get(position).cloned())?
        };
        order
            .get(style.active)
            .cloned()
            .or_else(|| order.last().cloned())
            .map(|p| (p, style.shown))
    }

    pub fn set_shown(&self, position: &PanelPosition, shown: bool) {
        self.styles.update(|styles| {
            if let Some(style) = styles.get_mut(position) {
                style.shown = shown;
            }
        });
    }

    pub fn toggle_active_maximize(&self) {
        let focus = self.common.focus.get_untracked();
        if let Focus::Panel(kind) = focus {
            if let Some((_, pos)) = self.panel_position(&kind) {
                if pos.is_bottom() {
                    self.toggle_bottom_maximize();
                }
            }
        }
    }

    pub fn toggle_maximize(&self, kind: &PanelKind) {
        if let Some((_, p)) = self.panel_position(kind) {
            if p.is_bottom() {
                self.toggle_bottom_maximize();
            }
        }
    }

    /// Toggles maximize for both bottom positions at once, because the bottom
    /// container is a single horizontal band that spans both BottomLeft and BottomRight.
    pub fn toggle_bottom_maximize(&self) {
        let maximized = !self.panel_bottom_maximized(false);
        self.styles.update(|styles| {
            if let Some(style) = styles.get_mut(&PanelPosition::BottomLeft) {
                style.maximized = maximized;
            }
            if let Some(style) = styles.get_mut(&PanelPosition::BottomRight) {
                style.maximized = maximized;
            }
        });
    }

    pub fn panel_bottom_maximized(&self, tracked: bool) -> bool {
        let styles = if tracked {
            self.styles.get()
        } else {
            self.styles.get_untracked()
        };
        styles
            .get(&PanelPosition::BottomLeft)
            .map(|p| p.maximized)
            .unwrap_or(false)
            || styles
                .get(&PanelPosition::BottomRight)
                .map(|p| p.maximized)
                .unwrap_or(false)
    }

    /// Toggles the visibility of an entire panel container (Left, Bottom, or Right).
    /// When hiding, calls `hide_panel` on each position's active panel first
    /// (to handle peer-position cleanup), then explicitly sets both positions to
    /// hidden as a safety net. The `hide_panel` calls handle peer-empty logic
    /// and the final `styles.update` ensures both positions are definitively hidden
    /// regardless of the order `hide_panel` processes them.
    pub fn toggle_container_visual(&self, position: &PanelContainerPosition) {
        let is_hidden = !self.is_container_shown(position, false);
        if is_hidden {
            self.styles.update(|styles| {
                let style = styles.entry(position.first()).or_default();
                style.shown = true;
                let style = styles.entry(position.second()).or_default();
                style.shown = true;
            });
        } else {
            if let Some((kind, _)) =
                self.active_panel_at_position(&position.second(), false)
            {
                self.hide_panel(&kind);
            }
            if let Some((kind, _)) =
                self.active_panel_at_position(&position.first(), false)
            {
                self.hide_panel(&kind);
            }
            self.styles.update(|styles| {
                let style = styles.entry(position.first()).or_default();
                style.shown = false;
                let style = styles.entry(position.second()).or_default();
                style.shown = false;
            });
        }
    }

    /// Returns the signal controlling whether a panel section is folded open or closed.
    /// Creates the signal lazily (defaulting to open) if it has not been persisted,
    /// which handles newly added sections that have no saved state.
    ///
    /// Note: Lazily created signals are never cleaned up, but this is acceptable
    /// because `PanelSection` is a fixed small enum (not dynamically generated),
    /// so the number of signals is bounded by the number of enum variants.
    pub fn section_open(&self, section: PanelSection) -> RwSignal<bool> {
        let open = self
            .sections
            .with_untracked(|sections| sections.get(&section).cloned());
        if let Some(open) = open {
            return open;
        }

        let open = self.common.scope.create_rw_signal(true);
        self.sections.update(|sections| {
            sections.insert(section, open);
        });
        open
    }
}

pub fn panel_position(
    order: &PanelOrder,
    kind: &PanelKind,
) -> Option<(usize, PanelPosition)> {
    for (pos, panels) in order.iter() {
        let index = panels.iter().position(|k| k == kind);
        if let Some(index) = index {
            return Some((index, *pos));
        }
    }
    None
}
