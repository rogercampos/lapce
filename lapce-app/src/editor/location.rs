use std::path::PathBuf;

use floem::peniko::kurbo::Vec2;
use lapce_core::{buffer::rope_text::RopeText, rope_text_pos::RopeTextPosition};
use lsp_types::Position;

/// A target location for editor navigation (go-to-definition, open file, etc.).
/// Used by `JumpToLocation` internal command to open files and position the cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct EditorLocation {
    pub path: PathBuf,
    pub position: Option<EditorPosition>,
    pub scroll_offset: Option<Vec2>,
    /// When true, the navigation should reuse the current editor tab rather than
    /// searching across all editor tabs for one already showing this file.
    pub same_editor_tab: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorPosition {
    Line(usize),
    Position(Position),
    Offset(usize),
}

impl EditorPosition {
    pub fn to_offset(&self, text: &impl RopeText) -> usize {
        match self {
            EditorPosition::Line(n) => text.first_non_blank_character_on_line(*n),
            EditorPosition::Position(position) => text.offset_of_position(position),
            EditorPosition::Offset(offset) => *offset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lapce_core::buffer::rope_text::RopeTextRef;
    use lapce_xi_rope::Rope;

    #[test]
    fn offset_variant_returns_directly() {
        let rope = Rope::from("hello world");
        let rt = RopeTextRef::new(&rope);
        let pos = EditorPosition::Offset(5);
        assert_eq!(pos.to_offset(&rt), 5);
    }

    #[test]
    fn offset_variant_zero() {
        let rope = Rope::from("abc");
        let rt = RopeTextRef::new(&rope);
        assert_eq!(EditorPosition::Offset(0).to_offset(&rt), 0);
    }

    #[test]
    fn position_variant_first_line() {
        let rope = Rope::from("hello\nworld");
        let rt = RopeTextRef::new(&rope);
        let pos = EditorPosition::Position(Position {
            line: 0,
            character: 3,
        });
        assert_eq!(pos.to_offset(&rt), 3);
    }

    #[test]
    fn position_variant_second_line() {
        let rope = Rope::from("hello\nworld");
        let rt = RopeTextRef::new(&rope);
        let pos = EditorPosition::Position(Position {
            line: 1,
            character: 2,
        });
        // Line 1 starts at offset 6, character 2 => offset 8
        assert_eq!(pos.to_offset(&rt), 8);
    }

    #[test]
    fn line_variant_no_indentation() {
        let rope = Rope::from("hello\nworld");
        let rt = RopeTextRef::new(&rope);
        let pos = EditorPosition::Line(0);
        // first_non_blank_character_on_line(0) => 0
        assert_eq!(pos.to_offset(&rt), 0);
    }

    #[test]
    fn line_variant_with_indentation() {
        let rope = Rope::from("    hello\nworld");
        let rt = RopeTextRef::new(&rope);
        let pos = EditorPosition::Line(0);
        // first_non_blank_character_on_line(0) => 4 (skips spaces)
        assert_eq!(pos.to_offset(&rt), 4);
    }

    #[test]
    fn line_variant_second_line_indented() {
        let rope = Rope::from("hello\n  world");
        let rt = RopeTextRef::new(&rope);
        let pos = EditorPosition::Line(1);
        // Line 1 starts at 6, first non-blank at offset 8
        assert_eq!(pos.to_offset(&rt), 8);
    }

    #[test]
    fn editor_position_equality() {
        assert_eq!(EditorPosition::Offset(5), EditorPosition::Offset(5));
        assert_ne!(EditorPosition::Offset(5), EditorPosition::Line(5));
    }
}
