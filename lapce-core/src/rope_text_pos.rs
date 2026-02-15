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

#[cfg(test)]
mod tests {
    use floem_editor_core::buffer::rope_text::RopeTextRef;
    use lapce_xi_rope::Rope;
    use lsp_types::Position;

    use super::RopeTextPosition;

    /// Helper: create a rope and run a closure with a RopeTextRef.
    fn with_rope<F, R>(s: &str, f: F) -> R
    where
        F: FnOnce(RopeTextRef<'_>) -> R,
    {
        let rope = Rope::from(s);
        let rt = RopeTextRef::new(&rope);
        f(rt)
    }

    // --- offset_to_position ---

    #[test]
    fn offset_to_position_start_of_text() {
        with_rope("hello\nworld", |rt| {
            let pos = rt.offset_to_position(0);
            assert_eq!(pos, Position::new(0, 0));
        });
    }

    #[test]
    fn offset_to_position_middle_of_first_line() {
        with_rope("hello\nworld", |rt| {
            let pos = rt.offset_to_position(3);
            assert_eq!(pos, Position::new(0, 3));
        });
    }

    #[test]
    fn offset_to_position_end_of_first_line() {
        with_rope("hello\nworld", |rt| {
            let pos = rt.offset_to_position(5);
            assert_eq!(pos, Position::new(0, 5));
        });
    }

    #[test]
    fn offset_to_position_start_of_second_line() {
        with_rope("hello\nworld", |rt| {
            let pos = rt.offset_to_position(6);
            assert_eq!(pos, Position::new(1, 0));
        });
    }

    #[test]
    fn offset_to_position_middle_of_second_line() {
        with_rope("hello\nworld", |rt| {
            let pos = rt.offset_to_position(9);
            assert_eq!(pos, Position::new(1, 3));
        });
    }

    #[test]
    fn offset_to_position_end_of_text() {
        with_rope("hello\nworld", |rt| {
            let pos = rt.offset_to_position(11);
            assert_eq!(pos, Position::new(1, 5));
        });
    }

    #[test]
    fn offset_to_position_empty_string() {
        with_rope("", |rt| {
            let pos = rt.offset_to_position(0);
            assert_eq!(pos, Position::new(0, 0));
        });
    }

    #[test]
    fn offset_to_position_single_newline() {
        with_rope("\n", |rt| {
            assert_eq!(rt.offset_to_position(0), Position::new(0, 0));
            assert_eq!(rt.offset_to_position(1), Position::new(1, 0));
        });
    }

    #[test]
    fn offset_to_position_multiple_lines() {
        with_rope("ab\ncd\nef", |rt| {
            assert_eq!(rt.offset_to_position(0), Position::new(0, 0));
            assert_eq!(rt.offset_to_position(2), Position::new(0, 2));
            assert_eq!(rt.offset_to_position(3), Position::new(1, 0));
            assert_eq!(rt.offset_to_position(5), Position::new(1, 2));
            assert_eq!(rt.offset_to_position(6), Position::new(2, 0));
            assert_eq!(rt.offset_to_position(8), Position::new(2, 2));
        });
    }

    #[test]
    fn offset_to_position_with_cjk_chars() {
        // CJK characters are 3 bytes in UTF-8 but 1 UTF-16 code unit each
        // (BMP range, no surrogates)
        with_rope("a\u{4e16}\u{754c}", |rt| {
            // 'a' at offset 0 -> position (0, 0)
            assert_eq!(rt.offset_to_position(0), Position::new(0, 0));
            // After 'a' (1 byte), start of '世' -> position (0, 1)
            assert_eq!(rt.offset_to_position(1), Position::new(0, 1));
            // After 'a' + '世' (1 + 3 = 4 bytes), start of '界' -> position (0, 2)
            assert_eq!(rt.offset_to_position(4), Position::new(0, 2));
            // After all (1 + 3 + 3 = 7 bytes) -> position (0, 3)
            assert_eq!(rt.offset_to_position(7), Position::new(0, 3));
        });
    }

    #[test]
    fn offset_to_position_with_emoji_surrogate_pair() {
        // Emoji like 😀 is U+1F600, 4 bytes UTF-8, 2 UTF-16 code units (surrogate pair)
        with_rope("a\u{1F600}b", |rt| {
            // 'a' at offset 0
            assert_eq!(rt.offset_to_position(0), Position::new(0, 0));
            // After 'a', start of emoji -> position (0, 1)
            assert_eq!(rt.offset_to_position(1), Position::new(0, 1));
            // After 'a' + emoji (1 + 4 = 5), start of 'b' -> position (0, 3)
            // (emoji takes 2 UTF-16 code units)
            assert_eq!(rt.offset_to_position(5), Position::new(0, 3));
        });
    }

    #[test]
    fn offset_to_position_second_line_with_multibyte() {
        // Test UTF-16 conversion on a non-first line
        with_rope("hi\n\u{1F600}end", |rt| {
            // start of second line (offset 3)
            assert_eq!(rt.offset_to_position(3), Position::new(1, 0));
            // after emoji on second line (offset 3+4=7)
            assert_eq!(rt.offset_to_position(7), Position::new(1, 2));
            // 'e' on second line (offset 7)
            // 'end' starts at offset 7, position (1, 2) because emoji = 2 utf16 units
        });
    }

    // --- offset_of_position (roundtrip) ---

    #[test]
    fn offset_of_position_roundtrip_ascii() {
        with_rope("hello\nworld\n!", |rt| {
            for offset in [0, 3, 5, 6, 10, 11, 12] {
                let pos = rt.offset_to_position(offset);
                let recovered = rt.offset_of_position(&pos);
                assert_eq!(
                    offset, recovered,
                    "roundtrip failed for offset {offset}"
                );
            }
        });
    }

    #[test]
    fn offset_of_position_roundtrip_multibyte() {
        // Only test at valid character boundaries
        with_rope("a\u{4e16}\n\u{1F600}", |rt| {
            // Valid offsets: 0 (a), 1 (世 start), 4 (\n), 5 (emoji start), 9 (end)
            for offset in [0, 1, 4, 5, 9] {
                let pos = rt.offset_to_position(offset);
                let recovered = rt.offset_of_position(&pos);
                assert_eq!(
                    offset, recovered,
                    "roundtrip failed for offset {offset}"
                );
            }
        });
    }

    #[test]
    fn offset_of_position_explicit() {
        with_rope("abc\ndef", |rt| {
            assert_eq!(rt.offset_of_position(&Position::new(0, 0)), 0);
            assert_eq!(rt.offset_of_position(&Position::new(0, 2)), 2);
            assert_eq!(rt.offset_of_position(&Position::new(1, 0)), 4);
            assert_eq!(rt.offset_of_position(&Position::new(1, 3)), 7);
        });
    }

    // --- position_to_line_col ---

    #[test]
    fn position_to_line_col_ascii() {
        with_rope("abc\ndef", |rt| {
            assert_eq!(rt.position_to_line_col(&Position::new(0, 0)), (0, 0));
            assert_eq!(rt.position_to_line_col(&Position::new(0, 2)), (0, 2));
            assert_eq!(rt.position_to_line_col(&Position::new(1, 0)), (1, 0));
            assert_eq!(rt.position_to_line_col(&Position::new(1, 1)), (1, 1));
        });
    }

    #[test]
    fn position_to_line_col_with_multibyte() {
        // '世' is 3 bytes UTF-8, 1 UTF-16 code unit
        with_rope("a\u{4e16}b", |rt| {
            // Position (0, 0) -> col 0
            assert_eq!(rt.position_to_line_col(&Position::new(0, 0)), (0, 0));
            // Position (0, 1) -> col 1 (after 'a', 1 byte)
            assert_eq!(rt.position_to_line_col(&Position::new(0, 1)), (0, 1));
            // Position (0, 2) -> col 4 (after 'a' + '世', 1 + 3 = 4 bytes)
            assert_eq!(rt.position_to_line_col(&Position::new(0, 2)), (0, 4));
        });
    }

    #[test]
    fn position_to_line_col_emoji() {
        // 😀 is 4 bytes UTF-8, 2 UTF-16 code units
        with_rope("\u{1F600}x", |rt| {
            // Position (0, 0) -> col 0
            assert_eq!(rt.position_to_line_col(&Position::new(0, 0)), (0, 0));
            // Position (0, 2) -> col 4 (after emoji: 4 bytes, 2 utf16 code units)
            assert_eq!(rt.position_to_line_col(&Position::new(0, 2)), (0, 4));
            // Position (0, 3) -> col 5 (after emoji + 'x')
            assert_eq!(rt.position_to_line_col(&Position::new(0, 3)), (0, 5));
        });
    }
}
