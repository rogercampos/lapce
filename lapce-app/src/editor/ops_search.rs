//! Find, search, and replace operations on [`EditorData`].
//!
//! Extracted from `editor.rs` as a separate `impl EditorData` block. Private
//! methods that the rest of the `editor` module dispatches to (via
//! `run_focus_command`) are upgraded to `pub(super)`.

use floem::{
    ext_event::create_ext_action,
    keyboard::Modifiers,
    reactive::{SignalGet, SignalUpdate, SignalWith},
};
use lapce_core::{buffer::rope_text::RopeText, selection::Selection};
use lapce_xi_rope::Rope;

use crate::{
    editor::{EditorData, InlineFindDirection},
    find::{Find, FindProgress},
};

impl EditorData {
    pub(super) fn search_whole_word_forward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let (word, buffer) = self.doc().buffer.with_untracked(|buffer| {
            let (start, end) = buffer.select_word(offset);
            (buffer.slice_to_cow(start..end).to_string(), buffer.clone())
        });
        if let Some(find_ed) = self.find_state.find_editor_signal.get_untracked() {
            find_ed.doc().reload(Rope::from(word.as_str()), true);
            let len = find_ed.doc().buffer.with_untracked(|b| b.len());
            find_ed
                .cursor()
                .update(|c| c.set_insert(Selection::region(0, len)));
        }
        let next = self
            .find_state
            .find
            .next(buffer.text(), offset, false, true);

        if let Some((start, _end)) = next {
            self.run_move_command(
                &lapce_core::movement::Movement::Offset(start),
                None,
                mods,
            );
        }
    }

    pub(super) fn search_forward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let text = self
            .doc()
            .buffer
            .with_untracked(|buffer| buffer.text().clone());
        let next = self.find_state.find.next(&text, offset, false, true);

        if let Some((start, _end)) = next {
            self.run_move_command(
                &lapce_core::movement::Movement::Offset(start),
                None,
                mods,
            );
        }
    }

    pub(super) fn search_backward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let text = self
            .doc()
            .buffer
            .with_untracked(|buffer| buffer.text().clone());
        let next = self.find_state.find.next(&text, offset, true, true);

        if let Some((start, _end)) = next {
            self.run_move_command(
                &lapce_core::movement::Movement::Offset(start),
                None,
                mods,
            );
        }
    }

    pub(super) fn replace_next(&self, text: &str) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let buffer = self.doc().buffer.with_untracked(|buffer| buffer.clone());
        // Use saturating_sub(1) so find.next() includes a match starting exactly
        // at the cursor position (find.next requires start > offset).
        let next = self.find_state.find.next(
            buffer.text(),
            offset.saturating_sub(1),
            false,
            true,
        );

        if let Some((start, end)) = next {
            let selection = Selection::region(start, end);
            self.do_edit(&selection, &[(selection.clone(), text)]);
            self.find_state.find.rev.update(|rev| *rev += 1);
        }
    }

    pub(super) fn replace_all(&self, text: &str) {
        let offset = self.cursor().with_untracked(|c| c.offset());

        self.update_find();

        let edits: Vec<(Selection, &str)> = self
            .find_state
            .find_result
            .occurrences
            .get_untracked()
            .regions()
            .iter()
            .map(|region| (Selection::region(region.start, region.end), text))
            .collect();
        if !edits.is_empty() {
            self.do_edit(&Selection::caret(offset), &edits);
            self.find_state.find.rev.update(|rev| *rev += 1);
        }
    }

    pub(super) fn replace_next_and_advance(&self) {
        if let Some(replace_ed) =
            self.find_state.replace_editor_signal.get_untracked()
        {
            let text = replace_ed.doc().buffer.with_untracked(|b| b.to_string());
            self.replace_next(&text);
            self.search_forward(Modifiers::empty());
        }
    }

    pub(super) fn replace_all_from_command(&self) {
        if let Some(replace_ed) =
            self.find_state.replace_editor_signal.get_untracked()
        {
            let text = replace_ed.doc().buffer.with_untracked(|b| b.to_string());
            self.replace_all(&text);
        }
    }

    pub fn clear_search(&self) {
        self.find_state.find.visual.set(false);
        self.find_state.find_focus.set(false);
    }

    pub(super) fn search(&self) {
        let pattern = self.word_at_cursor();

        let pattern = if pattern.contains('\n') || pattern.is_empty() {
            None
        } else {
            Some(pattern)
        };

        if let Some(find_ed) = self.find_state.find_editor_signal.get_untracked() {
            if let Some(ref p) = pattern {
                find_ed.doc().reload(Rope::from(p.as_str()), true);
            }
            // Always select all text in the find editor so the user can
            // immediately type to replace the previous search term.
            let len = find_ed.doc().buffer.with_untracked(|b| b.len());
            find_ed
                .cursor()
                .update(|c| c.set_insert(Selection::region(0, len)));
        }
        self.find_state.find.visual.set(true);
        self.find_state.find_focus.set(true);
        self.find_state.find.replace_active.set(false);
        self.find_state.find.replace_focus.set(false);
    }

    pub(super) fn search_and_replace(&self) {
        let pattern = self.word_at_cursor();

        let pattern = if pattern.contains('\n') || pattern.is_empty() {
            None
        } else {
            Some(pattern)
        };

        if let Some(find_ed) = self.find_state.find_editor_signal.get_untracked() {
            if let Some(ref p) = pattern {
                find_ed.doc().reload(Rope::from(p.as_str()), true);
            }
            let len = find_ed.doc().buffer.with_untracked(|b| b.len());
            find_ed
                .cursor()
                .update(|c| c.set_insert(Selection::region(0, len)));
        }
        self.find_state.find.visual.set(true);
        self.find_state.find_focus.set(true);
        self.find_state.find.replace_active.set(true);
        self.find_state.find.replace_focus.set(false);
    }

    /// Execute the find search on the current document's full text.
    /// Called from the paint path to update find results before rendering highlights.
    pub fn update_find(&self) {
        let find_rev = self.find_state.find.rev.get_untracked();
        if self.find_state.find_result.find_rev.get_untracked() != find_rev {
            if self
                .find_state
                .find
                .search_string
                .with_untracked(|search_string| {
                    search_string
                        .as_ref()
                        .map(|s| s.content.is_empty())
                        .unwrap_or(true)
                })
            {
                self.find_state
                    .find_result
                    .occurrences
                    .set(Selection::new());
            }
            self.find_state.find_result.reset();
            self.find_state.find_result.find_rev.set(find_rev);
        }

        if self.find_state.find_result.progress.get_untracked()
            != FindProgress::Started
        {
            return;
        }

        let search = self.find_state.find.search_string.get_untracked();
        let search = match search {
            Some(search) => search,
            None => return,
        };
        if search.content.is_empty() {
            return;
        }

        self.find_state
            .find_result
            .progress
            .set(FindProgress::InProgress(Selection::new()));

        let find_result = self.find_state.find_result.clone();
        let send = create_ext_action(self.scope, move |occurrences: Selection| {
            find_result.occurrences.set(occurrences);
            find_result.progress.set(FindProgress::Ready);
        });

        let text = self.doc().buffer.with_untracked(|b| b.text().clone());
        let case_matching = self.find_state.find.case_matching.get_untracked();
        let whole_words = self.find_state.find.whole_words.get_untracked();
        rayon::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut occurrences = Selection::new();
                    Find::find(
                        &text,
                        &search,
                        0,
                        text.len(),
                        case_matching,
                        whole_words,
                        false,
                        &mut occurrences,
                    );
                    send(occurrences);
                }));
            if let Err(e) = result {
                let msg = e
                    .downcast_ref::<String>()
                    .map(|s| s.as_str())
                    .or_else(|| e.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown");
                tracing::error!("Find panicked: {msg}");
            }
        });
    }

    /// Jump to the next/previous column on the line which matches the given text.
    pub(super) fn inline_find(&self, direction: InlineFindDirection, c: &str) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let doc = self.doc();
        let (line_content, line_start_offset) =
            doc.buffer.with_untracked(|buffer| {
                let line = buffer.line_of_offset(offset);
                let line_content = buffer.line_content(line);
                let line_start_offset = buffer.offset_of_line(line);
                (line_content.to_string(), line_start_offset)
            });
        let index = offset - line_start_offset;
        if let Some(new_index) = match direction {
            InlineFindDirection::Left => {
                line_content.get(..index).and_then(|s| s.rfind(c))
            }
            InlineFindDirection::Right => {
                if index + 1 >= line_content.len() {
                    None
                } else {
                    let index = index
                        + doc.buffer.with_untracked(|buffer| {
                            buffer.next_grapheme_offset(
                                offset,
                                1,
                                buffer.offset_line_end(offset, false),
                            )
                        })
                        - offset;
                    line_content
                        .get(index..)
                        .and_then(|s| s.find(c).map(|i| i + index))
                }
            }
        } {
            self.run_move_command(
                &lapce_core::movement::Movement::Offset(
                    new_index + line_start_offset,
                ),
                None,
                Modifiers::empty(),
            );
        }
    }

    /// Deactivate the on-screen find overlay and clear its pattern/regions.
    pub(super) fn quit_on_screen_find(&self) {
        if self.find_state.on_screen_find.with_untracked(|s| s.active) {
            self.find_state.on_screen_find.update(|f| {
                f.active = false;
                f.pattern.clear();
                f.regions.clear();
            })
        }
    }
}
