use floem_editor_core::buffer::rope_text::RopeText;
use lsp_types::Position;

use crate::encoding::{offset_utf8_to_utf16, offset_utf16_to_utf8};

/// Extension trait that adds LSP Position conversion methods to any RopeText.
/// LSP positions use (line, utf16_column), while the internal rope uses
/// byte offsets. This trait bridges the two coordinate systems.
pub trait RopeTextPosition: RopeText {
    /// Converts a UTF8 byte offset to an LSP Position (line + utf16 column).
    /// First finds the line, then converts the byte column offset within that
    /// line to utf16 code units for LSP compatibility.
    fn offset_to_position(&self, offset: usize) -> Position {
        let (line, col) = self.offset_to_line_col(offset);
        let line_offset = self.offset_of_line(line);

        let utf16_col =
            offset_utf8_to_utf16(self.char_indices_iter(line_offset..), col);

        Position {
            line: line as u32,
            character: utf16_col as u32,
        }
    }

    fn offset_of_position(&self, pos: &Position) -> usize {
        let (line, column) = self.position_to_line_col(pos);

        self.offset_of_line_col(line, column)
    }

    fn position_to_line_col(&self, pos: &Position) -> (usize, usize) {
        let line = pos.line as usize;
        let line_offset = self.offset_of_line(line);

        let column = offset_utf16_to_utf8(
            self.char_indices_iter(line_offset..),
            pos.character as usize,
        );

        (line, column)
    }
}
impl<T: RopeText> RopeTextPosition for T {}
