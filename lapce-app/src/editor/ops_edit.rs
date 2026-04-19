//! Core text-edit primitives on [`EditorData`]: applying a selection edit,
//! applying an LSP [`TextEdit`] list, and propagating deltas to snippet
//! placeholder offsets so placeholders survive edits made by completion.

use floem::reactive::{SignalGet, SignalUpdate, SignalWith};
use lapce_core::{
    buffer::InvalLines,
    editor::EditType,
    rope_text_pos::RopeTextPosition,
    selection::{InsertDrift, Selection},
};
use lapce_xi_rope::{Rope, RopeDelta, Transformer};
use lsp_types::TextEdit;

use crate::editor::EditorData;

impl EditorData {
    pub fn do_edit(
        &self,
        selection: &Selection,
        edits: &[(impl AsRef<Selection>, &str)],
    ) {
        let mut cursor = self.cursor().get_untracked();
        let doc = self.doc();
        let (text, delta, inval_lines) =
            match doc.do_raw_edit(edits, EditType::Completion) {
                Some(e) => e,
                None => return,
            };
        let selection = selection.apply_delta(&delta, true, InsertDrift::Default);
        let old_cursor = cursor.mode.clone();
        doc.buffer.update(|buffer| {
            cursor.update_selection(buffer, selection);
            buffer.set_cursor_before(old_cursor);
            buffer.set_cursor_after(cursor.mode.clone());
        });
        self.cursor().set(cursor);

        self.apply_deltas(&[(text, delta, inval_lines)]);
    }

    pub fn do_text_edit(&self, edits: &[TextEdit]) {
        let (selection, edits) = self.doc().buffer.with_untracked(|buffer| {
            let selection = self.cursor().get_untracked().edit_selection(buffer);
            let edits = edits
                .iter()
                .map(|edit| {
                    let selection = lapce_core::selection::Selection::region(
                        buffer.offset_of_position(&edit.range.start),
                        buffer.offset_of_position(&edit.range.end),
                    );
                    (selection, edit.new_text.as_str())
                })
                .collect::<Vec<_>>();
            (selection, edits)
        });

        self.do_edit(&selection, &edits);
    }

    /// Apply editor-level side effects of text deltas. The Doc has already applied
    /// its own updates (styles, diagnostics, completion lens, proxy sync) in
    /// `Doc::apply_deltas`. This method handles the EditorData-specific concern
    /// of keeping snippet placeholder offsets in sync with the text changes.
    pub(super) fn apply_deltas(&self, deltas: &[(Rope, RopeDelta, InvalLines)]) {
        for (_, delta, _) in deltas {
            self.update_snippet_offset(delta);
        }
    }

    fn update_snippet_offset(&self, delta: &RopeDelta) {
        if self.snippet.with_untracked(|s| s.is_some()) {
            self.snippet.update(|snippet| {
                let Some(current) = snippet.as_ref() else {
                    return;
                };
                let mut transformer = Transformer::new(delta);
                *snippet = Some(
                    current
                        .iter()
                        .map(|(tab, (start, end))| {
                            (
                                *tab,
                                (
                                    transformer.transform(*start, false),
                                    transformer.transform(*end, true),
                                ),
                            )
                        })
                        .collect(),
                );
            });
        }
    }
}
