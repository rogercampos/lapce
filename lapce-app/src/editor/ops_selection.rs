//! Selection helpers on [`EditorData`]. `selected_text` returns only what's
//! actively selected (empty string when the cursor is a caret), while
//! `word_at_cursor` widens a caret to the surrounding word — useful for
//! pre-filling the find input or building a rename payload.

use floem::reactive::SignalWith;
use lapce_core::buffer::rope_text::RopeText;
use tracing::instrument;

use crate::editor::EditorData;

impl EditorData {
    /// Returns the currently selected text, or an empty string if nothing is
    /// selected. Unlike `word_at_cursor()`, this does NOT expand to the word
    /// under the cursor when there is no selection.
    #[instrument]
    pub fn selected_text(&self) -> String {
        let doc = self.doc();
        let region = self.cursor().with_untracked(|c| match &c.mode {
            lapce_core::cursor::CursorMode::Normal(_) => None,
            lapce_core::cursor::CursorMode::Visual {
                start,
                end,
                mode: _,
            } => Some(lapce_core::selection::SelRegion::new(
                *start.min(end),
                doc.buffer.with_untracked(|buffer| {
                    buffer.next_grapheme_offset(*start.max(end), 1, buffer.len())
                }),
                None,
            )),
            lapce_core::cursor::CursorMode::Insert(selection) => {
                let region = *selection.last_inserted().unwrap();
                if region.is_caret() {
                    None
                } else {
                    Some(region)
                }
            }
        });

        match region {
            Some(region) => doc.buffer.with_untracked(|buffer| {
                buffer.slice_to_cow(region.min()..region.max()).to_string()
            }),
            None => String::new(),
        }
    }

    #[instrument]
    pub fn word_at_cursor(&self) -> String {
        let doc = self.doc();
        let region = self.cursor().with_untracked(|c| match &c.mode {
            lapce_core::cursor::CursorMode::Normal(offset) => {
                lapce_core::selection::SelRegion::caret(*offset)
            }
            lapce_core::cursor::CursorMode::Visual {
                start,
                end,
                mode: _,
            } => lapce_core::selection::SelRegion::new(
                *start.min(end),
                doc.buffer.with_untracked(|buffer| {
                    buffer.next_grapheme_offset(*start.max(end), 1, buffer.len())
                }),
                None,
            ),
            lapce_core::cursor::CursorMode::Insert(selection) => {
                *selection.last_inserted().unwrap()
            }
        });

        if region.is_caret() {
            doc.buffer.with_untracked(|buffer| {
                let (start, end) = buffer.select_word(region.start);
                buffer.slice_to_cow(start..end).to_string()
            })
        } else {
            doc.buffer.with_untracked(|buffer| {
                buffer.slice_to_cow(region.min()..region.max()).to_string()
            })
        }
    }
}
