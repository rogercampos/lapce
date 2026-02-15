use std::str;

use lapce_rpc::style::{LineStyle, Style};
use lapce_xi_rope::{LinesMetric, Rope, spans::Spans};

/// The set of recognized tree-sitter highlight scope names. These are matched
/// against capture names in tree-sitter query files (e.g., `@keyword`, `@type`).
/// The index of each scope in this array becomes the `Highlight(usize)` value,
/// which is then used to look up the corresponding theme color at render time.
///
/// Order matters: longer prefix matches are preferred, so more specific scopes
/// like "type.builtin" must appear alongside their parent "type".
pub const SCOPES: &[&str] = &[
    "constant",
    "type",
    "type.builtin",
    "property",
    "comment",
    "constructor",
    "function",
    "label",
    "keyword",
    "string",
    "variable",
    "variable.other.member",
    "operator",
    "attribute",
    "escape",
    "embedded",
    "symbol",
    "punctuation",
    "punctuation.special",
    "punctuation.delimiter",
    "text",
    "text.literal",
    "text.title",
    "text.uri",
    "text.reference",
    "string.escape",
    "conceal",
    "none",
    "tag",
    "markup.bold",
    "markup.italic",
    "markup.list",
    "markup.quote",
    "markup.heading",
    "markup.link.url",
    "markup.link.label",
    "markup.link.text",
];

/// Extracts the syntax highlighting styles for a single line from the
/// document-wide Spans structure. Converts absolute byte offsets to
/// line-relative column offsets for rendering.
pub fn line_styles(
    text: &Rope,
    line: usize,
    styles: &Spans<Style>,
) -> Vec<LineStyle> {
    let max_line = text.measure::<LinesMetric>() + 1;

    if line >= max_line {
        return Vec::new();
    }

    let start_offset = text.offset_of_line(line);
    let end_offset = text.offset_of_line(line + 1);
    let line_styles: Vec<LineStyle> = styles
        .iter_chunks(start_offset..end_offset)
        .filter_map(|(iv, style)| {
            let start = iv.start();
            let end = iv.end();
            if start > end_offset || end < start_offset {
                None
            } else {
                let start = start.saturating_sub(start_offset);
                let end = end - start_offset;
                let style = style.clone();
                Some(LineStyle { start, end, style })
            }
        })
        .collect();
    line_styles
}

#[cfg(test)]
mod tests {
    use super::*;
    use lapce_xi_rope::Rope;
    use lapce_xi_rope::spans::SpansBuilder;

    fn make_style(color: &str) -> Style {
        Style {
            fg_color: Some(color.to_string()),
        }
    }

    #[test]
    fn empty_text_returns_empty() {
        let rope = Rope::from("");
        let spans = SpansBuilder::<Style>::new(0).build();
        let result = line_styles(&rope, 0, &spans);
        assert!(result.is_empty());
    }

    #[test]
    fn out_of_bounds_line_returns_empty() {
        let rope = Rope::from("hello");
        let spans = SpansBuilder::<Style>::new(5).build();
        // line 5 doesn't exist in a single-line text
        let result = line_styles(&rope, 5, &spans);
        assert!(result.is_empty());
    }

    #[test]
    fn no_styles_returns_empty() {
        let rope = Rope::from("hello\nworld");
        let spans = SpansBuilder::<Style>::new(rope.len()).build();
        let result = line_styles(&rope, 0, &spans);
        assert!(result.is_empty());
    }

    #[test]
    fn single_style_on_first_line() {
        let text = "hello\nworld";
        let rope = Rope::from(text);
        let mut sb = SpansBuilder::<Style>::new(rope.len());
        // Style bytes 0..5 ("hello") on line 0
        sb.add_span(0..5, make_style("red"));
        let spans = sb.build();

        let result = line_styles(&rope, 0, &spans);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start, 0);
        assert_eq!(result[0].end, 5);
        assert_eq!(result[0].style.fg_color, Some("red".to_string()));
    }

    #[test]
    fn style_on_second_line() {
        let text = "hello\nworld";
        let rope = Rope::from(text);
        let mut sb = SpansBuilder::<Style>::new(rope.len());
        // Style bytes 6..11 ("world") on line 1
        sb.add_span(6..11, make_style("blue"));
        let spans = sb.build();

        // Line 1 should have style with line-relative offsets
        let result1 = line_styles(&rope, 1, &spans);
        assert_eq!(result1.len(), 1);
        assert_eq!(result1[0].start, 0); // 6 - 6 = 0
        assert_eq!(result1[0].end, 5); // 11 - 6 = 5
    }

    #[test]
    fn style_crossing_line_boundary() {
        let text = "aaa\nbbb";
        let rope = Rope::from(text);
        let mut sb = SpansBuilder::<Style>::new(rope.len());
        // Style from middle of line 0 to middle of line 1: bytes 1..6
        sb.add_span(1..6, make_style("green"));
        let spans = sb.build();

        // Line 0: start_offset=0, end_offset=4
        // iter_chunks returns the full span interval [1..6]; line_styles
        // converts to line-relative offsets without clamping to line boundary
        let result0 = line_styles(&rope, 0, &spans);
        assert_eq!(result0.len(), 1);
        assert_eq!(result0[0].start, 1);
        assert_eq!(result0[0].end, 6);

        // Line 1: start_offset=4, end_offset=7
        // [1..6] relative to line 1: start=0 (saturating_sub), end=2
        let result1 = line_styles(&rope, 1, &spans);
        assert_eq!(result1.len(), 1);
        assert_eq!(result1[0].start, 0);
        assert_eq!(result1[0].end, 2);
    }

    #[test]
    fn multiple_styles_on_one_line() {
        let text = "abcdef";
        let rope = Rope::from(text);
        let mut sb = SpansBuilder::<Style>::new(rope.len());
        sb.add_span(0..2, make_style("red"));
        sb.add_span(3..5, make_style("blue"));
        let spans = sb.build();

        let result = line_styles(&rope, 0, &spans);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].start, 0);
        assert_eq!(result[0].end, 2);
        assert_eq!(result[1].start, 3);
        assert_eq!(result[1].end, 5);
    }

    #[test]
    fn multiline_text_last_line() {
        let text = "aa\nbb\ncc";
        let rope = Rope::from(text);
        let mut sb = SpansBuilder::<Style>::new(rope.len());
        // Style the last line: bytes 6..8 ("cc")
        sb.add_span(6..8, make_style("purple"));
        let spans = sb.build();

        let result = line_styles(&rope, 2, &spans);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start, 0);
        assert_eq!(result[0].end, 2);
    }
}
