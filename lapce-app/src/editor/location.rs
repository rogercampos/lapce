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
