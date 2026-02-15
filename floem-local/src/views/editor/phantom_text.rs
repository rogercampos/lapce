use std::borrow::Cow;

use crate::{
    peniko::Color,
    text::{Attrs, AttrsList},
};
use floem_editor_core::cursor::CursorAffinity;
use smallvec::SmallVec;

/// `PhantomText` is for text that is not in the actual document, but should be rendered with it.
///
/// Ex: Inlay hints, IME text, error lens' diagnostics, etc
#[derive(Debug, Clone)]
pub struct PhantomText {
    /// The kind is currently used for sorting the phantom text on a line
    pub kind: PhantomTextKind,
    /// Column on the line that the phantom text should be displayed at
    pub col: usize,
    /// the affinity of cursor, e.g. for completion phantom text,
    /// we want the cursor always before the phantom text
    pub affinity: Option<CursorAffinity>,
    pub text: String,
    pub font_size: Option<usize>,
    // font_family: Option<FontFamily>,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub under_line: Option<Color>,
}

#[derive(Debug, Clone, Copy, Ord, Eq, PartialEq, PartialOrd)]
pub enum PhantomTextKind {
    /// Input methods
    Ime,
    Placeholder,
    /// Completion lens / Inline completion
    Completion,
    /// Inlay hints supplied by an LSP/PSP (like type annotations)
    InlayHint,
    /// Error lens
    Diagnostic,
}

/// Information about the phantom text on a specific line.
///
/// This has various utility functions for transforming a coordinate (typically a column) into the
/// resulting coordinate after the phantom text is combined with the line's real content.
#[derive(Debug, Default, Clone)]
pub struct PhantomTextLine {
    /// This uses a smallvec because most lines rarely have more than a couple phantom texts
    pub text: SmallVec<[PhantomText; 6]>,
}

impl PhantomTextLine {
    /// Translate a column position into the text into what it would be after combining
    pub fn col_at(&self, pre_col: usize) -> usize {
        let mut last = pre_col;
        for (col_shift, size, col, _) in self.offset_size_iter() {
            if pre_col >= col {
                last = pre_col + col_shift + size;
            }
        }

        last
    }

    /// Translate a column position into the text into what it would be after combining
    ///
    /// If `before_cursor` is false and the cursor is right at the start then it will stay there
    /// (Think 'is the phantom text before the cursor')
    pub fn col_after(&self, pre_col: usize, before_cursor: bool) -> usize {
        let mut last = pre_col;
        for (col_shift, size, col, text) in self.offset_size_iter() {
            let before_cursor = match text.affinity {
                Some(CursorAffinity::Forward) => true,
                Some(CursorAffinity::Backward) => false,
                None => before_cursor,
            };

            if pre_col > col || (pre_col == col && before_cursor) {
                last = pre_col + col_shift + size;
            }
        }

        last
    }

    /// Translate a column position into the text into what it would be after combining
    ///
    /// it only takes `before_cursor` in the params without considering the
    /// cursor affinity in phantom text
    pub fn col_after_force(&self, pre_col: usize, before_cursor: bool) -> usize {
        let mut last = pre_col;
        for (col_shift, size, col, _) in self.offset_size_iter() {
            if pre_col > col || (pre_col == col && before_cursor) {
                last = pre_col + col_shift + size;
            }
        }

        last
    }

    /// Translate a column position into the text into what it would be after combining
    ///
    /// If `before_cursor` is false and the cursor is right at the start then it will stay there
    ///
    /// (Think 'is the phantom text before the cursor')
    ///
    /// This accepts a `PhantomTextKind` to ignore. Primarily for IME due to it needing to put the
    /// cursor in the middle.
    pub fn col_after_ignore(
        &self,
        pre_col: usize,
        before_cursor: bool,
        skip: impl Fn(&PhantomText) -> bool,
    ) -> usize {
        let mut last = pre_col;
        for (col_shift, size, col, phantom) in self.offset_size_iter() {
            if skip(phantom) {
                continue;
            }

            if pre_col > col || (pre_col == col && before_cursor) {
                last = pre_col + col_shift + size;
            }
        }

        last
    }

    /// Translate a column position into the position it would be before combining
    pub fn before_col(&self, col: usize) -> usize {
        let mut last = col;
        for (col_shift, size, hint_col, _) in self.offset_size_iter() {
            let shifted_start = hint_col + col_shift;
            let shifted_end = shifted_start + size;
            if col >= shifted_start {
                if col >= shifted_end {
                    last = col - col_shift - size;
                } else {
                    last = hint_col;
                }
            }
        }
        last
    }

    /// Insert the hints at their positions in the text
    pub fn combine_with_text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut text = Cow::Borrowed(text);
        let mut col_shift = 0;

        for phantom in self.text.iter() {
            let location = phantom.col + col_shift;

            // Stop iterating if the location is bad
            if text.get(location..).is_none() {
                return text;
            }

            let mut text_o = text.into_owned();
            text_o.insert_str(location, &phantom.text);
            text = Cow::Owned(text_o);

            col_shift += phantom.text.len();
        }

        text
    }

    /// Iterator over `(col_shift, size, hint, pre_column)`
    /// Note that this only iterates over the ordered text, since those depend on the text for where
    /// they'll be positioned
    pub fn offset_size_iter(
        &self,
    ) -> impl Iterator<Item = (usize, usize, usize, &PhantomText)> + '_ {
        let mut col_shift = 0;

        self.text.iter().map(move |phantom| {
            let pre_col_shift = col_shift;
            col_shift += phantom.text.len();
            (
                pre_col_shift,
                col_shift - pre_col_shift,
                phantom.col,
                phantom,
            )
        })
    }

    pub fn apply_attr_styles(&self, default: Attrs, attrs_list: &mut AttrsList) {
        for (offset, size, col, phantom) in self.offset_size_iter() {
            let start = col + offset;
            let end = start + size;

            let mut attrs = default.clone();
            if let Some(fg) = phantom.fg {
                attrs = attrs.color(fg);
            }
            if let Some(phantom_font_size) = phantom.font_size {
                let font_size = attrs.font_size;
                attrs = attrs.font_size((phantom_font_size as f32).min(font_size));
            }

            attrs_list.add_span(start..end, attrs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn phantom(col: usize, text: &str) -> PhantomText {
        PhantomText {
            kind: PhantomTextKind::InlayHint,
            col,
            affinity: None,
            text: text.to_string(),
            font_size: None,
            fg: None,
            bg: None,
            under_line: None,
        }
    }

    fn phantom_with_affinity(
        col: usize,
        text: &str,
        affinity: Option<CursorAffinity>,
    ) -> PhantomText {
        PhantomText {
            affinity,
            ..phantom(col, text)
        }
    }

    fn line(phantoms: Vec<PhantomText>) -> PhantomTextLine {
        PhantomTextLine {
            text: SmallVec::from_vec(phantoms),
        }
    }

    fn empty_line() -> PhantomTextLine {
        PhantomTextLine { text: smallvec![] }
    }

    // ---- col_at ----

    #[test]
    fn col_at_no_phantoms() {
        let l = empty_line();
        assert_eq!(l.col_at(5), 5);
    }

    #[test]
    fn col_at_phantom_before() {
        // phantom at col 2, text ": i32" (5 chars)
        // col_at(5) -> 5 >= 2, so last = 5 + 0 + 5 = 10
        let l = line(vec![phantom(2, ": i32")]);
        assert_eq!(l.col_at(5), 10);
    }

    #[test]
    fn col_at_phantom_after() {
        // phantom at col 10, text ": i32" (5 chars)
        // col_at(5) -> 5 < 10, so last stays 5
        let l = line(vec![phantom(10, ": i32")]);
        assert_eq!(l.col_at(5), 5);
    }

    #[test]
    fn col_at_phantom_at_exact_col() {
        // phantom at col 5, text "hi" (2 chars)
        // col_at(5) -> 5 >= 5, so last = 5 + 0 + 2 = 7
        let l = line(vec![phantom(5, "hi")]);
        assert_eq!(l.col_at(5), 7);
    }

    #[test]
    fn col_at_multiple_phantoms() {
        // phantom at col 2 "ab" (2 chars), phantom at col 5 "cd" (2 chars)
        // col_at(6):
        //   phantom 0: col_shift=0, size=2, col=2. 6>=2 => last=6+0+2=8
        //   phantom 1: col_shift=2, size=2, col=5. 6>=5 => last=6+2+2=10
        let l = line(vec![phantom(2, "ab"), phantom(5, "cd")]);
        assert_eq!(l.col_at(6), 10);
    }

    #[test]
    fn col_at_col_between_phantoms() {
        // phantom at col 2 "ab", phantom at col 10 "cd"
        // col_at(5): 5>=2 => last=5+0+2=7, 5<10 => no change
        let l = line(vec![phantom(2, "ab"), phantom(10, "cd")]);
        assert_eq!(l.col_at(5), 7);
    }

    #[test]
    fn col_at_zero() {
        let l = line(vec![phantom(0, ">>")]);
        // col_at(0): 0>=0 => last=0+0+2=2
        assert_eq!(l.col_at(0), 2);
    }

    // ---- col_after ----

    #[test]
    fn col_after_no_phantoms() {
        let l = empty_line();
        assert_eq!(l.col_after(5, true), 5);
    }

    #[test]
    fn col_after_before_cursor_true() {
        // phantom at col 5, before_cursor=true
        // pre_col=5, 5==5 && true => last=5+0+2=7
        let l = line(vec![phantom(5, "hi")]);
        assert_eq!(l.col_after(5, true), 7);
    }

    #[test]
    fn col_after_before_cursor_false() {
        // phantom at col 5, before_cursor=false
        // pre_col=5, 5==5 && false => no match, last=5
        let l = line(vec![phantom(5, "hi")]);
        assert_eq!(l.col_after(5, false), 5);
    }

    #[test]
    fn col_after_affinity_forward_overrides() {
        // phantom with Forward affinity at col 5
        // Even with before_cursor=false, Forward affinity forces before_cursor=true
        let l = line(vec![phantom_with_affinity(
            5,
            "hi",
            Some(CursorAffinity::Forward),
        )]);
        assert_eq!(l.col_after(5, false), 7);
    }

    #[test]
    fn col_after_affinity_backward_overrides() {
        // phantom with Backward affinity at col 5
        // Even with before_cursor=true, Backward affinity forces before_cursor=false
        let l = line(vec![phantom_with_affinity(
            5,
            "hi",
            Some(CursorAffinity::Backward),
        )]);
        assert_eq!(l.col_after(5, true), 5);
    }

    #[test]
    fn col_after_pre_col_beyond() {
        // phantom at col 3, pre_col=10 > 3 => always matches
        let l = line(vec![phantom(3, "xyz")]);
        assert_eq!(l.col_after(10, false), 13);
    }

    // ---- col_after_force ----

    #[test]
    fn col_after_force_ignores_affinity() {
        // phantom with Forward affinity, before_cursor=false
        // Force ignores affinity, so 5==5 && false => no match
        let l = line(vec![phantom_with_affinity(
            5,
            "hi",
            Some(CursorAffinity::Forward),
        )]);
        assert_eq!(l.col_after_force(5, false), 5);
    }

    #[test]
    fn col_after_force_before_cursor_true() {
        let l = line(vec![phantom_with_affinity(
            5,
            "hi",
            Some(CursorAffinity::Backward),
        )]);
        // Force with before_cursor=true, ignores Backward affinity
        assert_eq!(l.col_after_force(5, true), 7);
    }

    // ---- col_after_ignore ----

    #[test]
    fn col_after_ignore_skips_ime() {
        let mut ime_phantom = phantom(3, "input");
        ime_phantom.kind = PhantomTextKind::Ime;
        let l = line(vec![ime_phantom, phantom(3, "hint")]);
        // Skip IME phantoms
        let result = l.col_after_ignore(5, true, |p| p.kind == PhantomTextKind::Ime);
        // Without IME (5 chars), only hint (4 chars at col 3) applies
        // But both are at col 3 < 5, so both would match.
        // IME is skipped. hint: col_shift=5(from IME), size=4, col=3
        // Actually, col_shift is cumulative from ALL phantoms in the iter,
        // including skipped ones. Let me reconsider.
        // offset_size_iter iterates all. skip only prevents the "last" update.
        // phantom 0 (IME): col_shift=0, size=5, col=3 => skipped
        // phantom 1 (hint): col_shift=5, size=4, col=3. 5>3 => last=5+5+4=14
        assert_eq!(result, 14);
    }

    #[test]
    fn col_after_ignore_skip_none() {
        let l = line(vec![phantom(3, "ab")]);
        let result = l.col_after_ignore(5, true, |_| false);
        // No skipping => same as col_after
        assert_eq!(result, l.col_after(5, true));
    }

    #[test]
    fn col_after_ignore_skip_all() {
        let l = line(vec![phantom(3, "ab")]);
        let result = l.col_after_ignore(5, true, |_| true);
        // All skipped => stays at pre_col
        assert_eq!(result, 5);
    }

    // ---- before_col ----

    #[test]
    fn before_col_no_phantoms() {
        let l = empty_line();
        assert_eq!(l.before_col(5), 5);
    }

    #[test]
    fn before_col_after_phantom() {
        // phantom at col 2, "hi" (2 chars)
        // shifted_start=2+0=2, shifted_end=2+2=4
        // col=5: 5>=2 and 5>=4 => last=5-0-2=3
        let l = line(vec![phantom(2, "hi")]);
        assert_eq!(l.before_col(5), 3);
    }

    #[test]
    fn before_col_inside_phantom() {
        // phantom at col 2, "abcd" (4 chars)
        // shifted_start=2+0=2, shifted_end=2+4=6
        // col=4: 4>=2 and 4<6 => last=hint_col=2
        let l = line(vec![phantom(2, "abcd")]);
        assert_eq!(l.before_col(4), 2);
    }

    #[test]
    fn before_col_before_phantom() {
        // phantom at col 5, "hi" (2 chars)
        // shifted_start=5, shifted_end=7
        // col=3: 3<5 => no change, last=3
        let l = line(vec![phantom(5, "hi")]);
        assert_eq!(l.before_col(3), 3);
    }

    #[test]
    fn before_col_roundtrip() {
        // col_at and before_col should be inverses for columns not inside phantoms
        let l = line(vec![phantom(3, "abc"), phantom(7, "de")]);
        for col in 0..15 {
            let combined = l.col_at(col);
            let back = l.before_col(combined);
            assert_eq!(
                back, col,
                "roundtrip failed: col={col}, combined={combined}, back={back}"
            );
        }
    }

    #[test]
    fn before_col_multiple_phantoms() {
        // phantom at col 2 "ab" (2), phantom at col 5 "cd" (2)
        // col=10:
        //   phantom 0: shift_start=2, shift_end=4. 10>=4 => last=10-0-2=8
        //   phantom 1: shift_start=5+2=7, shift_end=7+2=9. 10>=9 => last=10-2-2=6
        let l = line(vec![phantom(2, "ab"), phantom(5, "cd")]);
        assert_eq!(l.before_col(10), 6);
    }

    // ---- combine_with_text ----

    #[test]
    fn combine_no_phantoms() {
        let l = empty_line();
        let result = l.combine_with_text("hello");
        assert_eq!(&*result, "hello");
        // Should be a borrowed Cow
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn combine_single_phantom_at_start() {
        let l = line(vec![phantom(0, ">>")]);
        assert_eq!(&*l.combine_with_text("hello"), ">>hello");
    }

    #[test]
    fn combine_single_phantom_in_middle() {
        let l = line(vec![phantom(3, ": i32")]);
        assert_eq!(&*l.combine_with_text("let x = 5"), "let: i32 x = 5");
    }

    #[test]
    fn combine_single_phantom_at_end() {
        let l = line(vec![phantom(5, "<<")]);
        assert_eq!(&*l.combine_with_text("hello"), "hello<<");
    }

    #[test]
    fn combine_multiple_phantoms() {
        let l = line(vec![phantom(1, "A"), phantom(3, "B")]);
        // "hello" with A inserted at 1 => "hAello"
        // then B at 3 + shift(1) = 4 => "hAelBlo"
        assert_eq!(&*l.combine_with_text("hello"), "hAelBlo");
    }

    #[test]
    fn combine_phantom_beyond_text() {
        // phantom at col 100, text only 5 chars => early return
        let l = line(vec![phantom(100, "hi")]);
        assert_eq!(&*l.combine_with_text("hello"), "hello");
    }

    #[test]
    fn combine_empty_text() {
        let l = line(vec![phantom(0, "hint")]);
        assert_eq!(&*l.combine_with_text(""), "hint");
    }

    #[test]
    fn combine_empty_phantom_text() {
        let l = line(vec![phantom(2, "")]);
        assert_eq!(&*l.combine_with_text("hello"), "hello");
    }

    // ---- offset_size_iter ----

    #[test]
    fn offset_size_iter_empty() {
        let l = empty_line();
        let items: Vec<_> = l.offset_size_iter().collect();
        assert!(items.is_empty());
    }

    #[test]
    fn offset_size_iter_single() {
        let l = line(vec![phantom(3, "abc")]);
        let items: Vec<_> = l
            .offset_size_iter()
            .map(|(shift, size, col, _)| (shift, size, col))
            .collect();
        assert_eq!(items, vec![(0, 3, 3)]);
    }

    #[test]
    fn offset_size_iter_multiple() {
        let l = line(vec![phantom(2, "ab"), phantom(5, "cde")]);
        let items: Vec<_> = l
            .offset_size_iter()
            .map(|(shift, size, col, _)| (shift, size, col))
            .collect();
        // First: col_shift=0, size=2, col=2
        // Second: col_shift=2, size=3, col=5
        assert_eq!(items, vec![(0, 2, 2), (2, 3, 5)]);
    }

    #[test]
    fn offset_size_iter_cumulative_shift() {
        let l = line(vec![phantom(0, "a"), phantom(1, "bb"), phantom(2, "ccc")]);
        let items: Vec<_> = l
            .offset_size_iter()
            .map(|(shift, size, col, _)| (shift, size, col))
            .collect();
        assert_eq!(items, vec![(0, 1, 0), (1, 2, 1), (3, 3, 2)]);
    }

    // ---- PhantomTextKind ordering ----

    #[test]
    fn phantom_text_kind_ordering() {
        // Ime < Placeholder < Completion < InlayHint < Diagnostic
        assert!(PhantomTextKind::Ime < PhantomTextKind::Placeholder);
        assert!(PhantomTextKind::Placeholder < PhantomTextKind::Completion);
        assert!(PhantomTextKind::Completion < PhantomTextKind::InlayHint);
        assert!(PhantomTextKind::InlayHint < PhantomTextKind::Diagnostic);
    }
}
