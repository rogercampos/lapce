use floem::text::FamilyOwned;
use serde::{Deserialize, Serialize};
use structdesc::FieldNames;

#[derive(FieldNames, Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct UIConfig {
    #[field_names(desc = "Set the UI scale. Defaults to 1.0")]
    scale: f64,

    #[field_names(
        desc = "Set the UI font family. If empty, it uses system default."
    )]
    pub font_family: String,

    #[field_names(desc = "Set the UI base font size")]
    font_size: usize,

    #[field_names(desc = "Set the icon size in the UI")]
    icon_size: usize,

    #[field_names(
        desc = "Set the header height for panel header and editor tab header"
    )]
    header_height: usize,

    #[field_names(desc = "Set the height for status line")]
    status_height: usize,

    #[field_names(desc = "Set the minimum width for editor tab")]
    tab_min_width: usize,

    #[field_names(
        desc = "Set whether the editor tab separator should be full height or the height of the content"
    )]
    pub tab_separator_height: TabSeparatorHeight,

    #[field_names(desc = "Set the width for scroll bar")]
    scroll_width: usize,

    #[field_names(desc = "Controls the width of drop shadow in the UI")]
    drop_shadow_width: usize,

    #[field_names(desc = "Controls the width of the command palette")]
    palette_width: usize,

    #[field_names(
        desc = "Set the hover font family. If empty, it uses the UI font family"
    )]
    hover_font_family: String,
    #[field_names(desc = "Set the hover font size. If 0, uses the UI font size")]
    hover_font_size: usize,

    #[field_names(desc = "Trim whitespace from search results")]
    pub trim_search_results_whitespace: bool,

    #[field_names(desc = "Set the line height for list items")]
    list_line_height: usize,

    #[field_names(desc = "Set position of the close button in editor tabs")]
    pub tab_close_button: TabCloseButton,

    #[field_names(desc = "Display the Open Editors section in the explorer")]
    pub open_editors_visible: bool,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Deserialize,
    Serialize,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    strum_macros::VariantNames,
)]
pub enum TabCloseButton {
    Left,
    #[default]
    Right,
    Off,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Deserialize,
    Serialize,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    strum_macros::VariantNames,
)]
pub enum TabSeparatorHeight {
    #[default]
    Content,
    Full,
}

impl UIConfig {
    #[cfg(test)]
    fn test_default() -> Self {
        UIConfig {
            scale: 1.0,
            font_family: String::new(),
            font_size: 13,
            icon_size: 0,
            header_height: 36,
            status_height: 25,
            tab_min_width: 0,
            tab_separator_height: TabSeparatorHeight::default(),
            scroll_width: 10,
            drop_shadow_width: 0,
            palette_width: 0,
            hover_font_family: String::new(),
            hover_font_size: 0,
            trim_search_results_whitespace: false,
            list_line_height: 0,
            tab_close_button: TabCloseButton::default(),
            open_editors_visible: false,
        }
    }

    pub fn scale(&self) -> f64 {
        self.scale.clamp(0.1, 4.0)
    }

    pub fn font_size(&self) -> usize {
        self.font_size.clamp(6, 32)
    }

    pub fn font_family(&self) -> Vec<FamilyOwned> {
        FamilyOwned::parse_list(&self.font_family).collect()
    }

    pub fn header_height(&self) -> usize {
        let font_size = self.font_size();
        self.header_height.max(font_size)
    }

    pub fn icon_size(&self) -> usize {
        if self.icon_size == 0 {
            self.font_size()
        } else {
            self.icon_size.clamp(6, 32)
        }
    }

    pub fn status_height(&self) -> usize {
        let font_size = self.font_size();
        self.status_height.max(font_size)
    }

    pub fn palette_width(&self) -> usize {
        if self.palette_width == 0 {
            500
        } else {
            self.palette_width.max(100)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- scale() --

    #[test]
    fn scale_clamps_below_minimum() {
        let mut cfg = UIConfig::test_default();
        cfg.scale = 0.01;
        assert!((cfg.scale() - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn scale_clamps_above_maximum() {
        let mut cfg = UIConfig::test_default();
        cfg.scale = 10.0;
        assert!((cfg.scale() - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn scale_in_range_passes_through() {
        let mut cfg = UIConfig::test_default();
        cfg.scale = 1.5;
        assert!((cfg.scale() - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn scale_at_boundaries() {
        let mut cfg = UIConfig::test_default();
        cfg.scale = 0.1;
        assert!((cfg.scale() - 0.1).abs() < f64::EPSILON);
        cfg.scale = 4.0;
        assert!((cfg.scale() - 4.0).abs() < f64::EPSILON);
    }

    // -- font_size() --

    #[test]
    fn font_size_clamps_below_minimum() {
        let mut cfg = UIConfig::test_default();
        cfg.font_size = 1;
        assert_eq!(cfg.font_size(), 6);
    }

    #[test]
    fn font_size_clamps_above_maximum() {
        let mut cfg = UIConfig::test_default();
        cfg.font_size = 64;
        assert_eq!(cfg.font_size(), 32);
    }

    #[test]
    fn font_size_in_range_passes_through() {
        let mut cfg = UIConfig::test_default();
        cfg.font_size = 15;
        assert_eq!(cfg.font_size(), 15);
    }

    // -- header_height() --

    #[test]
    fn header_height_uses_value_when_above_font_size() {
        let mut cfg = UIConfig::test_default();
        cfg.font_size = 13;
        cfg.header_height = 36;
        assert_eq!(cfg.header_height(), 36);
    }

    #[test]
    fn header_height_clamps_to_font_size() {
        let mut cfg = UIConfig::test_default();
        cfg.font_size = 20;
        cfg.header_height = 10;
        assert_eq!(cfg.header_height(), 20);
    }

    // -- icon_size() --

    #[test]
    fn icon_size_zero_falls_back_to_font_size() {
        let mut cfg = UIConfig::test_default();
        cfg.font_size = 15;
        cfg.icon_size = 0;
        assert_eq!(cfg.icon_size(), 15);
    }

    #[test]
    fn icon_size_nonzero_in_range() {
        let mut cfg = UIConfig::test_default();
        cfg.icon_size = 18;
        assert_eq!(cfg.icon_size(), 18);
    }

    #[test]
    fn icon_size_clamps_below_minimum() {
        let mut cfg = UIConfig::test_default();
        cfg.icon_size = 2;
        assert_eq!(cfg.icon_size(), 6);
    }

    #[test]
    fn icon_size_clamps_above_maximum() {
        let mut cfg = UIConfig::test_default();
        cfg.icon_size = 50;
        assert_eq!(cfg.icon_size(), 32);
    }

    // -- status_height() --

    #[test]
    fn status_height_uses_value_when_above_font_size() {
        let mut cfg = UIConfig::test_default();
        cfg.font_size = 13;
        cfg.status_height = 25;
        assert_eq!(cfg.status_height(), 25);
    }

    #[test]
    fn status_height_clamps_to_font_size() {
        let mut cfg = UIConfig::test_default();
        cfg.font_size = 30;
        cfg.status_height = 10;
        assert_eq!(cfg.status_height(), 30);
    }

    // -- palette_width() --

    #[test]
    fn palette_width_zero_defaults_to_500() {
        let mut cfg = UIConfig::test_default();
        cfg.palette_width = 0;
        assert_eq!(cfg.palette_width(), 500);
    }

    #[test]
    fn palette_width_nonzero_passes_through() {
        let mut cfg = UIConfig::test_default();
        cfg.palette_width = 600;
        assert_eq!(cfg.palette_width(), 600);
    }

    #[test]
    fn palette_width_below_100_clamps() {
        let mut cfg = UIConfig::test_default();
        cfg.palette_width = 50;
        assert_eq!(cfg.palette_width(), 100);
    }

    #[test]
    fn palette_width_at_100_passes_through() {
        let mut cfg = UIConfig::test_default();
        cfg.palette_width = 100;
        assert_eq!(cfg.palette_width(), 100);
    }
}
