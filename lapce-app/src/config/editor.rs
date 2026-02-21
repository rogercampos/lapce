use floem::views::editor::text::RenderWhitespace;
use serde::{Deserialize, Serialize};
use structdesc::FieldNames;

pub const SCALE_OR_SIZE_LIMIT: f64 = 5.0;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum ClickMode {
    #[default]
    #[serde(rename = "single")]
    SingleClick,
    #[serde(rename = "file")]
    DoubleClickFile,
    #[serde(rename = "all")]
    DoubleClickAll,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WrapStyle {
    /// No wrapping
    None,
    /// Wrap at the editor width
    #[default]
    EditorWidth,
    // /// Wrap at the wrap-column
    // WrapColumn,
    /// Wrap at a specific width
    WrapWidth,
}
impl WrapStyle {
    pub fn as_str(&self) -> &'static str {
        match self {
            WrapStyle::None => "none",
            WrapStyle::EditorWidth => "editor-width",
            // WrapStyle::WrapColumn => "wrap-column",
            WrapStyle::WrapWidth => "wrap-width",
        }
    }

    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "none" => Some(WrapStyle::None),
            "editor-width" => Some(WrapStyle::EditorWidth),
            // "wrap-column" => Some(WrapStyle::WrapColumn),
            "wrap-width" => Some(WrapStyle::WrapWidth),
            _ => None,
        }
    }
}

impl std::fmt::Display for WrapStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())?;

        Ok(())
    }
}

#[derive(FieldNames, Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct EditorConfig {
    #[field_names(desc = "Set the editor font family")]
    pub font_family: String,
    #[field_names(desc = "Set the editor font size")]
    font_size: usize,
    #[field_names(desc = "Set the font size in the code glance")]
    pub code_glance_font_size: usize,
    #[field_names(
        desc = "Set the editor line height. If less than 5.0, line height will be a multiple of the font size."
    )]
    line_height: f64,
    #[field_names(
        desc = "If enabled, when you input a tab character, it will insert indent that's detected based on your files."
    )]
    pub smart_tab: bool,
    #[field_names(desc = "Set the tab width")]
    pub tab_width: usize,
    #[field_names(desc = "If opened editors are shown in a tab")]
    pub show_tab: bool,
    #[field_names(desc = "If navigation breadcrumbs are shown for the file")]
    pub show_bread_crumbs: bool,
    #[field_names(desc = "If the editor can scroll beyond the last line")]
    pub scroll_beyond_last_line: bool,
    #[field_names(
        desc = "Set the minimum number of visible lines above and below the cursor"
    )]
    pub cursor_surrounding_lines: usize,
    #[field_names(desc = "The kind of wrapping to perform")]
    pub wrap_style: WrapStyle,
    // #[field_names(desc = "The number of columns to wrap at")]
    // pub wrap_column: usize,
    #[field_names(desc = "The number of pixels to wrap at")]
    pub wrap_width: usize,
    #[field_names(
        desc = "Show code context like functions and classes at the top of editor when scroll"
    )]
    pub sticky_header: bool,
    #[field_names(desc = "The number of pixels to show completion")]
    pub completion_width: usize,
    #[field_names(
        desc = "If the editor should show the documentation of the current completion item"
    )]
    pub completion_show_documentation: bool,
    #[field_names(
        desc = "Should the completion item use the `detail` field to replace the label `field`?"
    )]
    pub completion_item_show_detail: bool,
    #[field_names(
        desc = "If the editor should show the signature of the function as the parameters are being typed"
    )]
    pub show_signature: bool,
    #[field_names(
        desc = "If the signature view should put the codeblock into a label. This might not work nicely for LSPs which provide invalid code for their labels."
    )]
    pub signature_label_code_block: bool,
    #[field_names(
        desc = "Whether the editor should enable automatic closing of matching pairs"
    )]
    pub auto_closing_matching_pairs: bool,
    #[field_names(
        desc = "Whether the editor should automatically surround selected text when typing quotes or brackets"
    )]
    pub auto_surround: bool,
    #[field_names(
        desc = "How long (in ms) it should take before the hover information appears"
    )]
    pub hover_delay: u64,
    #[field_names(
        desc = "Whether it should format the document on save (if there is an available formatter)"
    )]
    pub format_on_save: bool,

    #[field_names(
        desc = "Whether newlines should be automatically converted to the current line ending"
    )]
    pub normalize_line_endings: bool,

    #[field_names(desc = "If matching brackets are highlighted")]
    pub highlight_matching_brackets: bool,

    #[field_names(desc = "If scope lines are highlighted")]
    pub highlight_scope_lines: bool,

    #[field_names(desc = "If inlay hints should be displayed")]
    pub enable_inlay_hints: bool,

    #[field_names(
        desc = "Set the inlay hint font family. If empty, it uses the editor font family."
    )]
    pub inlay_hint_font_family: String,
    #[field_names(
        desc = "Set the inlay hint font size. If less than 5 or greater than editor font size, it uses the editor font size."
    )]
    pub inlay_hint_font_size: usize,
    #[field_names(desc = "If diagnostics should be displayed inline")]
    pub enable_error_lens: bool,

    #[field_names(
        desc = "Only render the styling without displaying messages, provided that `Enable ErrorLens` is enabled"
    )]
    pub only_render_error_styling: bool,
    #[field_names(
        desc = "Whether error lens should go to the end of view line, or only to the end of the diagnostic"
    )]
    pub error_lens_end_of_line: bool,
    #[field_names(
        desc = "Whether error lens should extend over multiple lines. If false, it will have newlines stripped."
    )]
    pub error_lens_multiline: bool,
    // TODO: Error lens but put entirely on the next line
    // TODO: error lens with indentation matching.
    #[field_names(
        desc = "Set error lens font family. If empty, it uses the inlay hint font family."
    )]
    pub error_lens_font_family: String,
    #[field_names(
        desc = "Set the error lens font size. If 0 it uses the inlay hint font size."
    )]
    pub error_lens_font_size: usize,
    #[field_names(
        desc = "If the editor should display the completion item as phantom text"
    )]
    pub enable_completion_lens: bool,
    #[field_names(desc = "If the editor should display inline completions")]
    pub enable_inline_completion: bool,
    #[field_names(
        desc = "Set completion lens font family. If empty, it uses the inlay hint font family."
    )]
    pub completion_lens_font_family: String,
    #[field_names(
        desc = "Set the completion lens font size. If 0 it uses the inlay hint font size."
    )]
    pub completion_lens_font_size: usize,
    #[field_names(
        desc = "Set the cursor blink interval (in milliseconds). Set to 0 to completely disable."
    )]
    blink_interval: u64,
    #[field_names(
        desc = "How the editor should render whitespace characters.\nOptions: none, all, boundary, trailing."
    )]
    pub render_whitespace: RenderWhitespace,
    #[field_names(desc = "Whether the editor show indent guide.")]
    pub show_indent_guide: bool,
    #[field_names(
        desc = "Set the auto save delay (in milliseconds), Set to 0 to completely disable"
    )]
    pub autosave_interval: u64,
    #[field_names(
        desc = "If enabled the cursor treats leading soft tabs as if they are hard tabs."
    )]
    pub atomic_soft_tabs: bool,
    #[field_names(
        desc = "Use a double click to interact with the file explorer.\nOptions: single (default), file or all."
    )]
    pub double_click: ClickMode,
    #[field_names(desc = "Move the focus as you type in the global search box")]
    pub move_focus_while_search: bool,
    #[field_names(
        desc = "Set the default number of visible lines above and below the diff block (-1 for infinite)"
    )]
    pub diff_context_lines: i32,
    #[field_names(desc = "Whether the editor colorizes brackets")]
    pub bracket_pair_colorization: bool,
    #[field_names(desc = "Bracket colorization Limit")]
    pub bracket_colorization_limit: u64,
    #[field_names(
        desc = "Glob patterns for excluding files and folders (in file explorer)"
    )]
    pub files_exclude: String,
    #[field_names(
        desc = "When enabled, all gems from Gemfile.lock are excluded from ruby-lsp indexing"
    )]
    pub ruby_lsp_exclude_gems: bool,
    #[field_names(desc = "Glob patterns to exclude from ruby-lsp indexing")]
    pub ruby_lsp_excluded_patterns: Vec<String>,
}

impl EditorConfig {
    #[cfg(test)]
    fn test_default() -> Self {
        EditorConfig {
            font_family: String::new(),
            font_size: 14,
            code_glance_font_size: 14,
            line_height: 1.6,
            smart_tab: false,
            tab_width: 4,
            show_tab: true,
            show_bread_crumbs: true,
            scroll_beyond_last_line: false,
            cursor_surrounding_lines: 1,
            wrap_style: WrapStyle::default(),
            wrap_width: 0,
            sticky_header: false,
            completion_width: 0,
            completion_show_documentation: false,
            completion_item_show_detail: false,
            show_signature: false,
            signature_label_code_block: false,
            auto_closing_matching_pairs: false,
            auto_surround: false,
            hover_delay: 300,
            format_on_save: false,
            normalize_line_endings: false,
            highlight_matching_brackets: false,
            highlight_scope_lines: false,
            enable_inlay_hints: false,
            inlay_hint_font_family: String::new(),
            inlay_hint_font_size: 0,
            enable_error_lens: false,
            only_render_error_styling: false,
            error_lens_end_of_line: false,
            error_lens_multiline: false,
            error_lens_font_family: String::new(),
            error_lens_font_size: 0,
            enable_completion_lens: false,
            enable_inline_completion: false,
            completion_lens_font_family: String::new(),
            completion_lens_font_size: 0,
            blink_interval: 500,
            render_whitespace: RenderWhitespace::default(),
            show_indent_guide: false,
            autosave_interval: 0,
            atomic_soft_tabs: false,
            double_click: ClickMode::default(),
            move_focus_while_search: false,
            diff_context_lines: 3,
            bracket_pair_colorization: false,
            bracket_colorization_limit: 0,
            files_exclude: String::new(),
            ruby_lsp_exclude_gems: true,
            ruby_lsp_excluded_patterns: Vec::new(),
        }
    }

    /// Clamps the font size to a safe range to prevent rendering issues.
    pub fn font_size(&self) -> usize {
        self.font_size.clamp(6, 32)
    }

    /// Interprets line_height as either a multiplier (when < 5.0, e.g. 1.5x) or
    /// an absolute pixel value (when >= 5.0). This dual interpretation is
    /// controlled by SCALE_OR_SIZE_LIMIT and matches the convention used by
    /// many editors.
    pub fn line_height(&self) -> usize {
        // Clamp to a minimum of 0.1 to prevent a zero line_height from producing
        // a zero result in multiplier mode (0.0 * font_size = 0.0).
        let clamped = self.line_height.max(0.1);
        let font_size = self.font_size();
        let line_height = if clamped < SCALE_OR_SIZE_LIMIT {
            clamped * font_size as f64
        } else {
            clamped
        };

        // Prevent overlapping lines
        (line_height.round() as usize).max(font_size)
    }

    pub fn inlay_hint_font_size(&self) -> usize {
        if self.inlay_hint_font_size < 5
            || self.inlay_hint_font_size > self.font_size()
        {
            self.font_size()
        } else {
            self.inlay_hint_font_size
        }
    }

    pub fn error_lens_font_size(&self) -> usize {
        if self.error_lens_font_size == 0 {
            self.inlay_hint_font_size()
        } else {
            self.error_lens_font_size
        }
    }

    pub fn completion_lens_font_size(&self) -> usize {
        if self.completion_lens_font_size == 0 {
            self.inlay_hint_font_size()
        } else {
            self.completion_lens_font_size
        }
    }

    /// Returns the tab width if atomic soft tabs are enabled.
    pub fn atomic_soft_tab_width(&self) -> Option<usize> {
        if self.atomic_soft_tabs {
            Some(self.tab_width)
        } else {
            None
        }
    }

    pub fn blink_interval(&self) -> u64 {
        if self.blink_interval == 0 {
            return 0;
        }
        self.blink_interval.max(200)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- font_size() --

    #[test]
    fn font_size_clamps_below_minimum() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 2;
        assert_eq!(cfg.font_size(), 6);
    }

    #[test]
    fn font_size_clamps_above_maximum() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 100;
        assert_eq!(cfg.font_size(), 32);
    }

    #[test]
    fn font_size_in_range_passes_through() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 16;
        assert_eq!(cfg.font_size(), 16);
    }

    #[test]
    fn font_size_at_boundaries() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 6;
        assert_eq!(cfg.font_size(), 6);
        cfg.font_size = 32;
        assert_eq!(cfg.font_size(), 32);
    }

    // -- line_height() --

    #[test]
    fn line_height_multiplier_mode() {
        // line_height < 5.0 → treated as a multiplier of font_size
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 10;
        cfg.line_height = 1.5;
        // 1.5 * 10 = 15.0 → round → 15
        assert_eq!(cfg.line_height(), 15);
    }

    #[test]
    fn line_height_absolute_mode() {
        // line_height >= 5.0 → treated as absolute pixel value
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 10;
        cfg.line_height = 24.0;
        assert_eq!(cfg.line_height(), 24);
    }

    #[test]
    fn line_height_at_boundary() {
        // Exactly 5.0 → absolute mode
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 10;
        cfg.line_height = 5.0;
        assert_eq!(cfg.line_height(), 10); // max(5, font_size=10)
    }

    #[test]
    fn line_height_just_below_boundary() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 10;
        cfg.line_height = 4.9;
        // 4.9 * 10 = 49.0 → multiplier mode
        assert_eq!(cfg.line_height(), 49);
    }

    #[test]
    fn line_height_never_below_font_size() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 20;
        cfg.line_height = 0.1;
        // 0.1 * 20 = 2.0, clamped to max(2, 20) = 20
        assert_eq!(cfg.line_height(), 20);
    }

    #[test]
    fn line_height_absolute_never_below_font_size() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 20;
        cfg.line_height = 8.0;
        // Absolute 8, but font_size is 20, so max(8, 20) = 20
        assert_eq!(cfg.line_height(), 20);
    }

    // -- inlay_hint_font_size() --

    #[test]
    fn inlay_hint_font_size_falls_back_when_too_small() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.inlay_hint_font_size = 3; // < 5
        assert_eq!(cfg.inlay_hint_font_size(), 14);
    }

    #[test]
    fn inlay_hint_font_size_falls_back_when_larger_than_font_size() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.inlay_hint_font_size = 20; // > font_size
        assert_eq!(cfg.inlay_hint_font_size(), 14);
    }

    #[test]
    fn inlay_hint_font_size_uses_value_in_valid_range() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.inlay_hint_font_size = 10;
        assert_eq!(cfg.inlay_hint_font_size(), 10);
    }

    #[test]
    fn inlay_hint_font_size_boundary_equal_to_font_size() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.inlay_hint_font_size = 14; // == font_size, NOT > font_size
        assert_eq!(cfg.inlay_hint_font_size(), 14);
    }

    #[test]
    fn inlay_hint_font_size_boundary_at_5() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.inlay_hint_font_size = 5; // >= 5 and <= font_size
        assert_eq!(cfg.inlay_hint_font_size(), 5);
    }

    // -- error_lens_font_size() --

    #[test]
    fn error_lens_font_size_zero_falls_back_to_inlay_hint() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.inlay_hint_font_size = 10;
        cfg.error_lens_font_size = 0;
        assert_eq!(cfg.error_lens_font_size(), 10);
    }

    #[test]
    fn error_lens_font_size_nonzero_passes_through() {
        let mut cfg = EditorConfig::test_default();
        cfg.error_lens_font_size = 12;
        assert_eq!(cfg.error_lens_font_size(), 12);
    }

    // -- completion_lens_font_size() --

    #[test]
    fn completion_lens_font_size_zero_falls_back_to_inlay_hint() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.inlay_hint_font_size = 10;
        cfg.completion_lens_font_size = 0;
        assert_eq!(cfg.completion_lens_font_size(), 10);
    }

    #[test]
    fn completion_lens_font_size_nonzero_passes_through() {
        let mut cfg = EditorConfig::test_default();
        cfg.completion_lens_font_size = 9;
        assert_eq!(cfg.completion_lens_font_size(), 9);
    }

    // -- atomic_soft_tab_width() --

    #[test]
    fn atomic_soft_tab_width_enabled() {
        let mut cfg = EditorConfig::test_default();
        cfg.atomic_soft_tabs = true;
        cfg.tab_width = 4;
        assert_eq!(cfg.atomic_soft_tab_width(), Some(4));
    }

    #[test]
    fn atomic_soft_tab_width_disabled() {
        let mut cfg = EditorConfig::test_default();
        cfg.atomic_soft_tabs = false;
        assert_eq!(cfg.atomic_soft_tab_width(), None);
    }

    // -- blink_interval() --

    #[test]
    fn blink_interval_zero_stays_zero() {
        let mut cfg = EditorConfig::test_default();
        cfg.blink_interval = 0;
        assert_eq!(cfg.blink_interval(), 0);
    }

    #[test]
    fn blink_interval_below_200_clamps() {
        let mut cfg = EditorConfig::test_default();
        cfg.blink_interval = 50;
        assert_eq!(cfg.blink_interval(), 200);
    }

    #[test]
    fn blink_interval_at_200_passes_through() {
        let mut cfg = EditorConfig::test_default();
        cfg.blink_interval = 200;
        assert_eq!(cfg.blink_interval(), 200);
    }

    #[test]
    fn blink_interval_above_200_passes_through() {
        let mut cfg = EditorConfig::test_default();
        cfg.blink_interval = 1000;
        assert_eq!(cfg.blink_interval(), 1000);
    }

    // -- WrapStyle --

    // -- line_height() zero / negative edge cases --

    #[test]
    fn line_height_zero_multiplier() {
        // Zero line_height in multiplier mode should be clamped to minimum
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.line_height = 0.0;
        // With the fix, 0.0 is clamped to 0.1, so 0.1 * 14 = 1.4 -> round -> 1
        // Then max(1, 14) = 14 (the font_size floor)
        assert_eq!(cfg.line_height(), 14);
        // Key point: it shouldn't panic or return 0
        assert!(cfg.line_height() > 0);
    }

    #[test]
    fn line_height_negative_value() {
        let mut cfg = EditorConfig::test_default();
        cfg.font_size = 14;
        cfg.line_height = -1.0;
        // Negative values are clamped by .max(0.1) -> 0.1 * 14 = 1.4 -> 1 -> max(1, 14) = 14
        assert_eq!(cfg.line_height(), 14);
    }

    // -- WrapStyle --

    #[test]
    fn wrap_style_as_str_roundtrip() {
        let variants = [
            WrapStyle::None,
            WrapStyle::EditorWidth,
            WrapStyle::WrapWidth,
        ];
        for v in variants {
            let s = v.as_str();
            let parsed = WrapStyle::try_from_str(s);
            assert_eq!(parsed, Some(v), "roundtrip failed for {s}");
        }
    }

    #[test]
    fn wrap_style_try_from_str_unknown_returns_none() {
        assert_eq!(WrapStyle::try_from_str("banana"), None);
        assert_eq!(WrapStyle::try_from_str(""), None);
    }

    #[test]
    fn wrap_style_display_matches_as_str() {
        let v = WrapStyle::EditorWidth;
        assert_eq!(format!("{v}"), v.as_str());
    }
}
