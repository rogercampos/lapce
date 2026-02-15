use floem_editor_core::buffer::{
    InsertsValueIter,
    rope_text::{RopeText, RopeTextRef},
};
use lapce_xi_rope::{
    Rope, RopeDelta, RopeInfo,
    delta::InsertDelta,
    multiset::{CountMatcher, Subset},
};
use tree_sitter::Point;

/// Wraps a sequence of tree-sitter InputEdits that describe how a text buffer
/// changed. Created from a xi-rope RopeDelta by factoring it into insertions
/// and deletions, then converting each to the tree-sitter edit format
/// (byte offsets + row/column positions).
#[derive(Clone)]
pub struct SyntaxEdit(pub(crate) Vec<tree_sitter::InputEdit>);

impl SyntaxEdit {
    pub fn new(edits: Vec<tree_sitter::InputEdit>) -> Self {
        Self(edits)
    }

    pub fn from_delta(text: &Rope, delta: RopeDelta) -> SyntaxEdit {
        let (ins_delta, deletes) = delta.factor();

        Self::from_factored_delta(text, &ins_delta, &deletes)
    }

    /// Converts a factored rope delta (insertions + deletions) into tree-sitter
    /// InputEdits. Edits are reversed so tree-sitter processes them from the end
    /// of the document backward, preventing earlier edits from invalidating the
    /// byte offsets of later edits.
    ///
    /// The delete subset is first transformed to account for the insertions,
    /// so delete positions are in the post-insertion coordinate space.
    pub fn from_factored_delta(
        text: &Rope,
        ins_delta: &InsertDelta<RopeInfo>,
        deletes: &Subset,
    ) -> SyntaxEdit {
        let deletes = deletes.transform_expand(&ins_delta.inserted_subset());

        let mut edits = Vec::new();

        let mut insert_edits: Vec<tree_sitter::InputEdit> =
            InsertsValueIter::new(ins_delta)
                .map(|insert| {
                    let start = insert.old_offset;
                    let inserted = insert.node;
                    create_insert_edit(text, start, inserted)
                })
                .collect();
        insert_edits.reverse();
        edits.append(&mut insert_edits);

        let text = ins_delta.apply(text);
        let mut delete_edits: Vec<tree_sitter::InputEdit> = deletes
            .range_iter(CountMatcher::NonZero)
            .map(|(start, end)| create_delete_edit(&text, start, end))
            .collect();
        delete_edits.reverse();
        edits.append(&mut delete_edits);

        SyntaxEdit::new(edits)
    }
}

/// Converts a byte offset within a Rope into a tree-sitter Point (row, column).
fn point_at_offset(text: &Rope, offset: usize) -> Point {
    let text = RopeTextRef::new(text);
    let line = text.line_of_offset(offset);
    let col = offset - text.offset_of_line(line);
    Point::new(line, col)
}

/// Advances a Point through a string, tracking newlines to update row/column.
/// Used to calculate the new end position after an insertion.
fn traverse(point: Point, text: &str) -> Point {
    let Point {
        mut row,
        mut column,
    } = point;

    for ch in text.chars() {
        if ch == '\n' {
            row += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    Point { row, column }
}

pub fn create_insert_edit(
    old_text: &Rope,
    start: usize,
    inserted: &Rope,
) -> tree_sitter::InputEdit {
    let start_position = point_at_offset(old_text, start);
    tree_sitter::InputEdit {
        start_byte: start,
        old_end_byte: start,
        new_end_byte: start + inserted.len(),
        start_position,
        old_end_position: start_position,
        new_end_position: traverse(
            start_position,
            &inserted.slice_to_cow(0..inserted.len()),
        ),
    }
}

pub fn create_delete_edit(
    old_text: &Rope,
    start: usize,
    end: usize,
) -> tree_sitter::InputEdit {
    let start_position = point_at_offset(old_text, start);
    let end_position = point_at_offset(old_text, end);
    tree_sitter::InputEdit {
        start_byte: start,
        // The old end byte position was at the end
        old_end_byte: end,
        // but since we're deleting everything up to it, it gets 'moved' to where we start
        new_end_byte: start,

        start_position,
        old_end_position: end_position,
        new_end_position: start_position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lapce_xi_rope::Rope;
    use tree_sitter::Point;

    // --- traverse ---

    #[test]
    fn traverse_empty_string() {
        let p = traverse(Point::new(0, 0), "");
        assert_eq!(p, Point::new(0, 0));
    }

    #[test]
    fn traverse_single_line() {
        let p = traverse(Point::new(0, 0), "hello");
        assert_eq!(p, Point::new(0, 5));
    }

    #[test]
    fn traverse_with_newline() {
        let p = traverse(Point::new(0, 0), "ab\ncd");
        assert_eq!(p, Point::new(1, 2));
    }

    #[test]
    fn traverse_multiple_newlines() {
        let p = traverse(Point::new(0, 0), "a\n\nc");
        assert_eq!(p, Point::new(2, 1));
    }

    #[test]
    fn traverse_ending_with_newline() {
        let p = traverse(Point::new(0, 0), "abc\n");
        assert_eq!(p, Point::new(1, 0));
    }

    #[test]
    fn traverse_from_nonzero_start() {
        let p = traverse(Point::new(3, 5), "xy\nz");
        assert_eq!(p, Point::new(4, 1));
    }

    // --- point_at_offset ---

    #[test]
    fn point_at_offset_start_of_text() {
        let rope = Rope::from("hello\nworld");
        assert_eq!(point_at_offset(&rope, 0), Point::new(0, 0));
    }

    #[test]
    fn point_at_offset_middle_of_first_line() {
        let rope = Rope::from("hello\nworld");
        assert_eq!(point_at_offset(&rope, 3), Point::new(0, 3));
    }

    #[test]
    fn point_at_offset_start_of_second_line() {
        let rope = Rope::from("hello\nworld");
        // offset 6 = 'w' on line 1
        assert_eq!(point_at_offset(&rope, 6), Point::new(1, 0));
    }

    #[test]
    fn point_at_offset_end_of_text() {
        let rope = Rope::from("ab\ncd");
        // offset 5 = end of text
        assert_eq!(point_at_offset(&rope, 5), Point::new(1, 2));
    }

    // --- create_insert_edit ---

    #[test]
    fn create_insert_edit_at_start() {
        let old = Rope::from("hello");
        let inserted = Rope::from("XX");
        let edit = create_insert_edit(&old, 0, &inserted);

        assert_eq!(edit.start_byte, 0);
        assert_eq!(edit.old_end_byte, 0);
        assert_eq!(edit.new_end_byte, 2);
        assert_eq!(edit.start_position, Point::new(0, 0));
        assert_eq!(edit.old_end_position, Point::new(0, 0));
        assert_eq!(edit.new_end_position, Point::new(0, 2));
    }

    #[test]
    fn create_insert_edit_multiline_insertion() {
        let old = Rope::from("ab");
        let inserted = Rope::from("x\ny");
        let edit = create_insert_edit(&old, 1, &inserted);

        assert_eq!(edit.start_byte, 1);
        assert_eq!(edit.old_end_byte, 1);
        assert_eq!(edit.new_end_byte, 4); // 1 + 3
        assert_eq!(edit.start_position, Point::new(0, 1));
        assert_eq!(edit.old_end_position, Point::new(0, 1));
        // traverse from (0,1) through "x\ny" => (1, 1)
        assert_eq!(edit.new_end_position, Point::new(1, 1));
    }

    // --- create_delete_edit ---

    #[test]
    fn create_delete_edit_single_line() {
        let old = Rope::from("hello world");
        let edit = create_delete_edit(&old, 5, 11);

        assert_eq!(edit.start_byte, 5);
        assert_eq!(edit.old_end_byte, 11);
        assert_eq!(edit.new_end_byte, 5);
        assert_eq!(edit.start_position, Point::new(0, 5));
        assert_eq!(edit.old_end_position, Point::new(0, 11));
        assert_eq!(edit.new_end_position, Point::new(0, 5));
    }

    #[test]
    fn create_delete_edit_across_lines() {
        let old = Rope::from("aa\nbb\ncc");
        // Delete from offset 1 (second char of line 0) to offset 6 (first char of line 2)
        let edit = create_delete_edit(&old, 1, 6);

        assert_eq!(edit.start_byte, 1);
        assert_eq!(edit.old_end_byte, 6);
        assert_eq!(edit.new_end_byte, 1);
        assert_eq!(edit.start_position, Point::new(0, 1));
        assert_eq!(edit.old_end_position, Point::new(2, 0));
        assert_eq!(edit.new_end_position, Point::new(0, 1));
    }
}
