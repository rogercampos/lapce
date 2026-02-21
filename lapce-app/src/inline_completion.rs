use std::{borrow::Cow, ops::Range, path::PathBuf, str::FromStr};

use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith, batch};
use lapce_core::{
    buffer::{
        Buffer,
        rope_text::{RopeText, RopeTextRef},
    },
    rope_text_pos::RopeTextPosition,
    selection::Selection,
};
use lsp_types::InsertTextFormat;

use crate::{config::LapceConfig, doc::Doc, editor::EditorData, snippet::Snippet};

// TODO: we could integrate completion lens with this, so it is considered at the same time

/// LSP inline completion item translated to use byte offsets instead of LSP positions.
/// This conversion happens once at receipt time (in `from_lsp`) to avoid repeated
/// position-to-offset lookups during rendering.
#[derive(Debug, Clone)]
pub struct InlineCompletionItem {
    /// The text to replace the range with.
    pub insert_text: String,
    /// Text used to decide if this inline completion should be shown.
    pub filter_text: Option<String>,
    /// The range (of offsets) to replace  
    pub range: Option<Range<usize>>,
    pub command: Option<lsp_types::Command>,
    pub insert_text_format: Option<InsertTextFormat>,
}
impl InlineCompletionItem {
    pub fn from_lsp(buffer: &Buffer, item: lsp_types::InlineCompletionItem) -> Self {
        let range = item.range.map(|r| {
            let start = buffer.offset_of_position(&r.start);
            let end = buffer.offset_of_position(&r.end);
            start..end
        });
        Self {
            insert_text: item.insert_text,
            filter_text: item.filter_text,
            range,
            command: item.command,
            insert_text_format: item.insert_text_format,
        }
    }

    pub fn apply(
        &self,
        editor: &EditorData,
        start_offset: usize,
    ) -> anyhow::Result<()> {
        let text_format = self
            .insert_text_format
            .unwrap_or(InsertTextFormat::PLAIN_TEXT);

        let selection = if let Some(range) = &self.range {
            Selection::region(range.start, range.end)
        } else {
            Selection::caret(start_offset)
        };

        match text_format {
            InsertTextFormat::PLAIN_TEXT => editor.do_edit(
                &selection,
                &[(selection.clone(), self.insert_text.as_str())],
            ),
            InsertTextFormat::SNIPPET => {
                editor.completion_apply_snippet(
                    &self.insert_text,
                    &selection,
                    Vec::new(),
                    start_offset,
                )?;
            }
            _ => {
                // We don't know how to support this text format
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineCompletionStatus {
    /// The inline completion is not active.
    Inactive,
    /// The inline completion is active and is waiting for the server to respond.
    Started,
    /// The inline completion is active and has received a response from the server.
    Active,
}

#[derive(Clone)]
pub struct InlineCompletionData {
    pub status: InlineCompletionStatus,
    /// The active inline completion index in the list of completions.
    pub active: RwSignal<usize>,
    pub items: im::Vector<InlineCompletionItem>,
    pub start_offset: usize,
    pub path: PathBuf,
}
impl InlineCompletionData {
    pub fn new(cx: Scope) -> Self {
        Self {
            status: InlineCompletionStatus::Inactive,
            active: cx.create_rw_signal(0),
            items: im::vector![],
            start_offset: 0,
            path: PathBuf::new(),
        }
    }

    pub fn current_item(&self) -> Option<&InlineCompletionItem> {
        let active = self.active.get_untracked();
        self.items.get(active)
    }

    pub fn next(&mut self) {
        if !self.items.is_empty() {
            let next_index = (self.active.get_untracked() + 1) % self.items.len();
            self.active.set(next_index);
        }
    }

    pub fn previous(&mut self) {
        if !self.items.is_empty() {
            let prev_index = if self.active.get_untracked() == 0 {
                self.items.len() - 1
            } else {
                self.active.get_untracked() - 1
            };
            self.active.set(prev_index);
        }
    }

    pub fn cancel(&mut self) {
        if self.status == InlineCompletionStatus::Inactive {
            return;
        }

        self.items.clear();
        self.status = InlineCompletionStatus::Inactive;
    }

    /// Set the items for the inline completion.  
    /// Sets `active` to `0` and `status` to `InlineCompletionStatus::Active`.
    pub fn set_items(
        &mut self,
        items: im::Vector<InlineCompletionItem>,
        start_offset: usize,
        path: PathBuf,
    ) {
        batch(|| {
            self.items = items;
            self.active.set(0);
            self.status = InlineCompletionStatus::Active;
            self.start_offset = start_offset;
            self.path = path;
        });
    }

    pub fn update_doc(&self, doc: &Doc, offset: usize) {
        if self.status != InlineCompletionStatus::Active {
            doc.clear_inline_completion();
            return;
        }

        if self.items.is_empty() {
            doc.clear_inline_completion();
            return;
        }

        let active = self.active.get_untracked();
        let active = if active >= self.items.len() {
            self.active.set(0);
            0
        } else {
            active
        };

        let Some(item) = self.items.get(active) else {
            doc.clear_inline_completion();
            return;
        };
        let text = item.insert_text.clone();

        // TODO: is range really meant to be used for this?
        let offset = item.range.as_ref().map(|r| r.start).unwrap_or(offset);
        let (line, col) = doc
            .buffer
            .with_untracked(|buffer| buffer.offset_to_line_col(offset));
        doc.set_inline_completion(text, line, col);
    }

    pub fn update_inline_completion(
        &self,
        config: &LapceConfig,
        doc: &Doc,
        cursor_offset: usize,
    ) {
        if !config.editor.enable_inline_completion {
            doc.clear_inline_completion();
            return;
        }

        let text = doc.buffer.with_untracked(|buffer| buffer.text().clone());
        let text = RopeTextRef::new(&text);
        let Some(item) = self.current_item() else {
            // TODO(minor): should we cancel completion
            return;
        };

        let completion = doc.inline_completion.with_untracked(|cur| {
            let cur = cur.as_deref();
            inline_completion_text(text, self.start_offset, cursor_offset, item, cur)
        });

        match completion {
            ICompletionRes::Hide => {
                doc.clear_inline_completion();
            }
            ICompletionRes::Unchanged => {}
            ICompletionRes::Set(new, shift) => {
                let offset = self.start_offset + shift;
                let (line, col) = text.offset_to_line_col(offset);
                doc.set_inline_completion(new, line, col);
            }
        }
    }
}

/// Result of computing inline completion display text.
/// Similar to the three-level return in completion_lens_text:
/// - `Hide` = no valid completion to show
/// - `Unchanged` = same text as currently displayed, skip DOM update
/// - `Set(text, shift)` = new ghost text to display, shifted by `shift` bytes from start_offset
enum ICompletionRes {
    Hide,
    Unchanged,
    Set(String, usize),
}

/// Get the text of the inline completion item  
fn inline_completion_text(
    rope_text: impl RopeText,
    start_offset: usize,
    cursor_offset: usize,
    item: &InlineCompletionItem,
    current_completion: Option<&str>,
) -> ICompletionRes {
    let text_format = item
        .insert_text_format
        .unwrap_or(InsertTextFormat::PLAIN_TEXT);

    // TODO: is this check correct? I mostly copied it from completion lens
    let cursor_prev_offset = rope_text.prev_code_boundary(cursor_offset);
    if let Some(range) = &item.range {
        let edit_start = range.start;

        // If the start of the edit isn't where the cursor currently is, and is not at the start of
        // the inline completion, then we ignore it.
        if cursor_prev_offset != edit_start && start_offset != edit_start {
            return ICompletionRes::Hide;
        }
    }

    let text = match text_format {
        InsertTextFormat::PLAIN_TEXT => Cow::Borrowed(&item.insert_text),
        InsertTextFormat::SNIPPET => {
            let Ok(snippet) = Snippet::from_str(&item.insert_text) else {
                return ICompletionRes::Hide;
            };
            let text = snippet.text();

            Cow::Owned(text)
        }
        _ => {
            // We don't know how to support this text format
            return ICompletionRes::Hide;
        }
    };

    // The prefix is the text from the completion start to the current cursor position,
    // representing what the user has already typed. We strip this from the completion text
    // so that, for example, `p` with a completion of `println` will show `rintln`.
    let range = start_offset..cursor_offset;
    let prefix = rope_text.slice_to_cow(range);
    let Some(text) = text.strip_prefix(prefix.as_ref()) else {
        return ICompletionRes::Hide;
    };

    if Some(text) == current_completion {
        ICompletionRes::Unchanged
    } else {
        ICompletionRes::Set(text.to_string(), prefix.len())
    }
}

#[cfg(test)]
mod tests {
    use lapce_core::buffer::rope_text::RopeTextRef;
    use lapce_xi_rope::Rope;
    use lsp_types::InsertTextFormat;

    use super::{ICompletionRes, InlineCompletionItem, inline_completion_text};

    /// Helper to create an InlineCompletionItem for tests.
    fn item(
        insert_text: &str,
        range: Option<std::ops::Range<usize>>,
        format: Option<InsertTextFormat>,
    ) -> InlineCompletionItem {
        InlineCompletionItem {
            insert_text: insert_text.to_string(),
            filter_text: None,
            range,
            command: None,
            insert_text_format: format,
        }
    }

    /// Helper: run inline_completion_text on a given buffer string.
    fn run(
        buffer: &str,
        start_offset: usize,
        cursor_offset: usize,
        completion: &InlineCompletionItem,
        current: Option<&str>,
    ) -> ICompletionRes {
        let rope = Rope::from(buffer);
        let rt = RopeTextRef::new(&rope);
        inline_completion_text(rt, start_offset, cursor_offset, completion, current)
    }

    // --- Plain text completions ---

    #[test]
    fn plain_text_strips_prefix() {
        // Buffer: "pr" on a line, completion is "println"
        // start_offset = 0, so prefix = "pr" (from offset 0 to end of line)
        let c = item("println", None, None);
        match run("pr", 0, 2, &c, None) {
            ICompletionRes::Set(text, shift) => {
                assert_eq!(text, "intln");
                assert_eq!(shift, 2);
            }
            other => {
                panic!("expected Set, got {:?}", std::mem::discriminant(&other))
            }
        }
    }

    #[test]
    fn plain_text_full_match_returns_empty_set() {
        // Buffer already contains the full completion text.
        // strip_prefix succeeds but returns "" — Set with empty string.
        let c = item("hello", None, None);
        match run("hello", 0, 5, &c, None) {
            ICompletionRes::Set(text, shift) => {
                assert_eq!(text, "");
                assert_eq!(shift, 5);
            }
            other => panic!(
                "expected Set with empty text, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn plain_text_no_prefix_match_hides() {
        // Buffer text doesn't match start of completion
        let c = item("println", None, None);
        assert!(matches!(run("xyz", 0, 3, &c, None), ICompletionRes::Hide));
    }

    #[test]
    fn plain_text_unchanged_when_same() {
        // Current completion matches what we'd compute
        let c = item("println", None, None);
        match run("pr", 0, 2, &c, Some("intln")) {
            ICompletionRes::Unchanged => {}
            other => panic!(
                "expected Unchanged, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn plain_text_set_when_different_from_current() {
        let c = item("println", None, None);
        match run("pr", 0, 2, &c, Some("old_value")) {
            ICompletionRes::Set(text, _) => {
                assert_eq!(text, "intln");
            }
            other => {
                panic!("expected Set, got {:?}", std::mem::discriminant(&other))
            }
        }
    }

    #[test]
    fn plain_text_empty_prefix() {
        // start_offset at beginning of empty content before newline
        let c = item("hello", None, None);
        match run("\n", 0, 0, &c, None) {
            ICompletionRes::Set(text, shift) => {
                assert_eq!(text, "hello");
                assert_eq!(shift, 0);
            }
            other => {
                panic!("expected Set, got {:?}", std::mem::discriminant(&other))
            }
        }
    }

    // --- Snippet completions ---

    #[test]
    fn snippet_strips_tabstops() {
        // Snippet: "println!(${1:msg})" -> plain text "println!(msg)"
        let c = item("println!(${1:msg})", None, Some(InsertTextFormat::SNIPPET));
        match run("pr", 0, 2, &c, None) {
            ICompletionRes::Set(text, shift) => {
                assert_eq!(text, "intln!(msg)");
                assert_eq!(shift, 2);
            }
            other => {
                panic!("expected Set, got {:?}", std::mem::discriminant(&other))
            }
        }
    }

    #[test]
    fn snippet_unparseable_produces_empty_text() {
        // The snippet parser can't extract elements from "${invalid" (no valid
        // tabstop number), so it produces an empty snippet whose text() is "".
        // Since prefix is also "" (buffer is empty), strip_prefix succeeds
        // with "", resulting in Set("", 0).
        let c = item("${invalid", None, Some(InsertTextFormat::SNIPPET));
        match run("", 0, 0, &c, None) {
            ICompletionRes::Set(text, shift) => {
                assert_eq!(text, "");
                assert_eq!(shift, 0);
            }
            other => {
                panic!(
                    "expected Set with empty text, got {:?}",
                    std::mem::discriminant(&other)
                )
            }
        }
    }

    // --- Range checks ---

    #[test]
    fn range_matching_cursor_prev_boundary() {
        // Range start matches cursor's prev_code_boundary
        // For "pr|", prev_code_boundary of offset 2 in "pr" should be 0
        // Range start = 0 matches start_offset = 0, so this should work
        let c = item("println", Some(0..2), None);
        match run("pr", 0, 2, &c, None) {
            ICompletionRes::Set(text, _) => {
                assert_eq!(text, "intln");
            }
            other => {
                panic!("expected Set, got {:?}", std::mem::discriminant(&other))
            }
        }
    }

    #[test]
    fn range_not_matching_hides() {
        // Range start (10) doesn't match cursor prev boundary or start_offset (0)
        let c = item("println", Some(10..15), None);
        assert!(matches!(run("pr", 0, 2, &c, None), ICompletionRes::Hide));
    }

    #[test]
    fn range_matches_start_offset() {
        // Range start == start_offset, so it should proceed
        let c = item("println", Some(0..5), None);
        match run("pr", 0, 2, &c, None) {
            ICompletionRes::Set(text, _) => {
                assert_eq!(text, "intln");
            }
            other => {
                panic!("expected Set, got {:?}", std::mem::discriminant(&other))
            }
        }
    }

    // --- Multiline buffer ---

    #[test]
    fn multiline_buffer_second_line() {
        // Completion on the second line
        // "first\npr" -> start_offset=6, prefix = "pr"
        let c = item("println", None, None);
        match run("first\npr", 6, 8, &c, None) {
            ICompletionRes::Set(text, shift) => {
                assert_eq!(text, "intln");
                assert_eq!(shift, 2);
            }
            other => {
                panic!("expected Set, got {:?}", std::mem::discriminant(&other))
            }
        }
    }

    // --- Default text format ---

    #[test]
    fn none_format_defaults_to_plain_text() {
        let c = item("hello", None, None);
        match run("he", 0, 2, &c, None) {
            ICompletionRes::Set(text, shift) => {
                assert_eq!(text, "llo");
                assert_eq!(shift, 2);
            }
            other => {
                panic!("expected Set, got {:?}", std::mem::discriminant(&other))
            }
        }
    }

    // --- InlineCompletionStatus ---

    #[test]
    fn status_enum_equality() {
        use super::InlineCompletionStatus;
        assert_eq!(
            InlineCompletionStatus::Inactive,
            InlineCompletionStatus::Inactive
        );
        assert_eq!(
            InlineCompletionStatus::Started,
            InlineCompletionStatus::Started
        );
        assert_eq!(
            InlineCompletionStatus::Active,
            InlineCompletionStatus::Active
        );
        assert_ne!(
            InlineCompletionStatus::Inactive,
            InlineCompletionStatus::Active
        );
        assert_ne!(
            InlineCompletionStatus::Started,
            InlineCompletionStatus::Active
        );
    }
}
