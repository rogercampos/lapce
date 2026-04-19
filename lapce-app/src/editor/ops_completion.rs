//! Autocompletion and inline-completion operations on [`EditorData`].
//!
//! Extracted from `editor.rs` as a separate `impl EditorData` block.
//! Methods that were previously private (`fn`) become `pub(super)` so the
//! rest of the `editor` module can still dispatch to them from
//! `run_focus_command` and the free functions at the bottom of `editor.rs`.

use std::str::FromStr;

use floem::{
    ext_event::create_ext_action,
    reactive::{SignalGet, SignalUpdate, SignalWith},
};
use lapce_core::{
    buffer::rope_text::RopeText,
    editor::EditType,
    rope_text_pos::RopeTextPosition,
    selection::{InsertDrift, Selection},
};
use lapce_rpc::proxy::ProxyResponse;
use lapce_xi_rope::Transformer;
use lsp_types::{CompletionItem, CompletionTextEdit, InlineCompletionTriggerKind};

use crate::{
    completion::CompletionStatus,
    editor::EditorData,
    inline_completion::{InlineCompletionItem, InlineCompletionStatus},
    snippet::Snippet,
};

impl EditorData {
    pub(super) fn select_inline_completion(&self) {
        if self
            .common
            .inline_completion
            .with_untracked(|c| c.status == InlineCompletionStatus::Inactive)
        {
            return;
        }

        let data = self
            .common
            .inline_completion
            .with_untracked(|c| (c.current_item().cloned(), c.start_offset));
        self.cancel_inline_completion();

        let (Some(item), start_offset) = data else {
            return;
        };

        if let Err(err) = item.apply(self, start_offset) {
            tracing::error!("{:?}", err);
        }
    }

    pub(super) fn next_inline_completion(&self) {
        if self
            .common
            .inline_completion
            .with_untracked(|c| c.status == InlineCompletionStatus::Inactive)
        {
            return;
        }

        self.common.inline_completion.update(|c| {
            c.next();
        });
    }

    pub(super) fn previous_inline_completion(&self) {
        if self
            .common
            .inline_completion
            .with_untracked(|c| c.status == InlineCompletionStatus::Inactive)
        {
            return;
        }

        self.common.inline_completion.update(|c| {
            c.previous();
        });
    }

    pub fn cancel_inline_completion(&self) {
        if self
            .common
            .inline_completion
            .with_untracked(|c| c.status == InlineCompletionStatus::Inactive)
        {
            return;
        }

        self.common.inline_completion.update(|c| {
            c.cancel();
        });

        self.doc().clear_inline_completion();
    }

    /// Update the current inline completion
    pub(super) fn update_inline_completion(
        &self,
        trigger_kind: InlineCompletionTriggerKind,
    ) {
        let doc = self.doc();
        let path = match doc.loaded_file_path() {
            Some(path) => path,
            None => return,
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let line = doc
            .buffer
            .with_untracked(|buffer| buffer.line_of_offset(offset));
        let position = doc
            .buffer
            .with_untracked(|buffer| buffer.offset_to_position(offset));

        let inline_completion = self.common.inline_completion;
        let doc = self.doc();

        // Update the inline completion's text if it's already active to avoid flickering
        let has_relevant = inline_completion.with_untracked(|completion| {
            let c_line = doc.buffer.with_untracked(|buffer| {
                buffer.line_of_offset(completion.start_offset)
            });
            completion.status != InlineCompletionStatus::Inactive
                && line == c_line
                && completion.path == path
        });
        if has_relevant {
            let config = self.common.config.get_untracked();
            inline_completion.update(|completion| {
                completion.update_inline_completion(&config, &doc, offset);
            });
        }

        let path2 = path.clone();
        let send = create_ext_action(
            self.scope,
            move |items: Vec<lsp_types::InlineCompletionItem>| {
                let items = doc.buffer.with_untracked(|buffer| {
                    items
                        .into_iter()
                        .map(|item| InlineCompletionItem::from_lsp(buffer, item))
                        .collect()
                });
                inline_completion.update(|c| {
                    c.set_items(items, offset, path2);
                    c.update_doc(&doc, offset);
                });
            },
        );

        inline_completion.update(|c| c.status = InlineCompletionStatus::Started);

        self.common.proxy.get_inline_completions(
            path,
            position,
            trigger_kind,
            move |res| {
                if let Ok(ProxyResponse::GetInlineCompletions {
                    completions: items,
                }) = res
                {
                    let items = match items {
                        lsp_types::InlineCompletionResponse::Array(items) => items,
                        // Currently does not have any relevant extra fields
                        lsp_types::InlineCompletionResponse::List(items) => {
                            items.items
                        }
                    };
                    send(items);
                }
            },
        );
    }

    /// Check if there are inline completions that are being rendered
    pub(super) fn has_inline_completions(&self) -> bool {
        self.common.inline_completion.with_untracked(|completion| {
            completion.status != InlineCompletionStatus::Inactive
                && !completion.items.is_empty()
        })
    }

    pub fn select_completion(&self) {
        let item = self
            .common
            .completion
            .with_untracked(|c| c.current_item().cloned());
        self.cancel_completion();
        let doc = self.doc();
        if let Some(item) = item {
            if item.item.data.is_some() {
                let editor = self.clone();
                let rev = doc.buffer.with_untracked(|buffer| buffer.rev());
                let path = doc.content.with_untracked(|c| c.path().cloned());
                let offset = self.cursor().with_untracked(|c| c.offset());
                let buffer = doc.buffer;
                let content = doc.content;
                let send = create_ext_action(self.scope, move |item| {
                    if editor.cursor().with_untracked(|c| c.offset() != offset) {
                        return;
                    }
                    if buffer.with_untracked(|b| b.rev()) != rev
                        || content.with_untracked(|content| {
                            content.path() != path.as_ref()
                        })
                    {
                        return;
                    }
                    if let Err(err) = editor.apply_completion_item(&item) {
                        tracing::error!("{:?}", err);
                    }
                });
                self.common.proxy.completion_resolve(
                    item.plugin_id,
                    item.item.clone(),
                    move |result| {
                        let item =
                            if let Ok(ProxyResponse::CompletionResolveResponse {
                                item,
                            }) = result
                            {
                                *item
                            } else {
                                item.item.clone()
                            };
                        send(item);
                    },
                );
            } else if let Err(err) = self.apply_completion_item(&item.item) {
                tracing::error!("{:?}", err);
            }
        }
    }

    pub fn cancel_completion(&self) {
        if self.common.completion.with_untracked(|c| c.status)
            == CompletionStatus::Inactive
        {
            return;
        }
        self.common.completion.update(|c| {
            c.cancel();
        });

        self.doc().clear_completion_lens()
    }

    /// Update the displayed autocompletion box
    /// Sends a request to the LSP for completion information
    pub(super) fn update_completion(&self, display_if_empty_input: bool) {
        let doc = self.doc();
        let path = match doc.loaded_file_path() {
            Some(path) => path,
            None => return,
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (start_offset, input, char) = doc.buffer.with_untracked(|buffer| {
            let start_offset = buffer.prev_code_boundary(offset);
            let end_offset = buffer.next_code_boundary(offset);
            let input = buffer.slice_to_cow(start_offset..end_offset).to_string();
            let char = if start_offset == 0 {
                "".to_string()
            } else {
                buffer
                    .slice_to_cow(start_offset - 1..start_offset)
                    .to_string()
            };
            (start_offset, input, char)
        });
        if !display_if_empty_input && input.is_empty() && char != "." && char != ":"
        {
            self.cancel_completion();
            return;
        }

        if self.common.completion.with_untracked(|completion| {
            completion.status != CompletionStatus::Inactive
                && completion.offset == start_offset
                && completion.path == path
        }) {
            self.common.completion.update(|completion| {
                completion.update_input(input.clone());

                if !completion.input_items.contains_key("") {
                    let start_pos = doc.buffer.with_untracked(|buffer| {
                        buffer.offset_to_position(start_offset)
                    });
                    completion.request(
                        self.id(),
                        &self.common.proxy,
                        path.clone(),
                        "".to_string(),
                        start_pos,
                    );
                }

                if !completion.input_items.contains_key(&input) {
                    let position = doc
                        .buffer
                        .with_untracked(|buffer| buffer.offset_to_position(offset));
                    completion.request(
                        self.id(),
                        &self.common.proxy,
                        path,
                        input,
                        position,
                    );
                }
            });
            let cursor_offset = self.cursor().with_untracked(|c| c.offset());
            self.common
                .completion
                .get_untracked()
                .update_document_completion(self, cursor_offset);

            return;
        }

        let doc = self.doc();
        self.common.completion.update(|completion| {
            completion.path.clone_from(&path);
            completion.offset = start_offset;
            completion.input.clone_from(&input);
            completion.status = CompletionStatus::Started;
            completion.input_items.clear();
            completion.request_id += 1;
            let start_pos = doc
                .buffer
                .with_untracked(|buffer| buffer.offset_to_position(start_offset));
            completion.request(
                self.id(),
                &self.common.proxy,
                path.clone(),
                "".to_string(),
                start_pos,
            );

            if !input.is_empty() {
                let position = doc
                    .buffer
                    .with_untracked(|buffer| buffer.offset_to_position(offset));
                completion.request(
                    self.id(),
                    &self.common.proxy,
                    path,
                    input,
                    position,
                );
            }
        });
    }

    /// Check if there are completions that are being rendered
    pub(super) fn has_completions(&self) -> bool {
        self.common.completion.with_untracked(|completion| {
            completion.status != CompletionStatus::Inactive
                && !completion.filtered_items.is_empty()
        })
    }

    fn apply_completion_item(&self, item: &CompletionItem) -> anyhow::Result<()> {
        let doc = self.doc();
        let buffer = doc.buffer.get_untracked();
        let cursor = self.cursor().get_untracked();
        // Get all the edits which would be applied in places other than right where the cursor is
        let additional_edit: Vec<_> = item
            .additional_text_edits
            .as_ref()
            .into_iter()
            .flatten()
            .map(|edit| {
                let selection = lapce_core::selection::Selection::region(
                    buffer.offset_of_position(&edit.range.start),
                    buffer.offset_of_position(&edit.range.end),
                );
                (selection, edit.new_text.as_str())
            })
            .collect::<Vec<(lapce_core::selection::Selection, &str)>>();

        let text_format = item
            .insert_text_format
            .unwrap_or(lsp_types::InsertTextFormat::PLAIN_TEXT);
        if let Some(edit) = &item.text_edit {
            match edit {
                CompletionTextEdit::Edit(edit) => {
                    let offset = cursor.offset();
                    let start_offset = buffer.prev_code_boundary(offset);
                    let end_offset = buffer.next_code_boundary(offset);
                    let edit_start = buffer.offset_of_position(&edit.range.start);
                    let edit_end = buffer.offset_of_position(&edit.range.end);

                    let selection = lapce_core::selection::Selection::region(
                        start_offset.min(edit_start),
                        end_offset.max(edit_end),
                    );
                    match text_format {
                        lsp_types::InsertTextFormat::PLAIN_TEXT => {
                            self.do_edit(
                                &selection,
                                &[
                                    &[(selection.clone(), edit.new_text.as_str())][..],
                                    &additional_edit[..],
                                ]
                                .concat(),
                            );
                            return Ok(());
                        }
                        lsp_types::InsertTextFormat::SNIPPET => {
                            self.completion_apply_snippet(
                                &edit.new_text,
                                &selection,
                                additional_edit,
                                start_offset,
                            )?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                CompletionTextEdit::InsertAndReplace(edit) => {
                    let offset = cursor.offset();
                    let start_offset = buffer.prev_code_boundary(offset);
                    let end_offset = buffer.next_code_boundary(offset);
                    let edit_start = buffer.offset_of_position(&edit.insert.start);
                    let edit_end = buffer.offset_of_position(&edit.insert.end);

                    let selection = lapce_core::selection::Selection::region(
                        start_offset.min(edit_start),
                        end_offset.max(edit_end),
                    );
                    match text_format {
                        lsp_types::InsertTextFormat::PLAIN_TEXT => {
                            self.do_edit(
                                &selection,
                                &[
                                    &[(
                                        selection.clone(),
                                        edit.new_text.as_str(),
                                    )][..],
                                    &additional_edit[..],
                                ]
                                .concat(),
                            );
                            return Ok(());
                        }
                        lsp_types::InsertTextFormat::SNIPPET => {
                            self.completion_apply_snippet(
                                &edit.new_text,
                                &selection,
                                additional_edit,
                                start_offset,
                            )?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }

        let offset = cursor.offset();
        let start_offset = buffer.prev_code_boundary(offset);
        let end_offset = buffer.next_code_boundary(offset);
        let selection = Selection::region(start_offset, end_offset);

        self.do_edit(
            &selection,
            &[
                &[(
                    selection.clone(),
                    item.insert_text.as_deref().unwrap_or(item.label.as_str()),
                )][..],
                &additional_edit[..],
            ]
            .concat(),
        );
        Ok(())
    }

    pub fn completion_apply_snippet(
        &self,
        snippet: &str,
        selection: &Selection,
        additional_edit: Vec<(Selection, &str)>,
        start_offset: usize,
    ) -> anyhow::Result<()> {
        let snippet = Snippet::from_str(snippet)?;
        let text = snippet.text();
        let mut cursor = self.cursor().get_untracked();
        let old_cursor = cursor.mode.clone();
        let (b_text, delta, inval_lines) = self
            .doc()
            .do_raw_edit(
                &[
                    &[(selection.clone(), text.as_str())][..],
                    &additional_edit[..],
                ]
                .concat(),
                EditType::Completion,
            )
            .ok_or_else(|| anyhow::anyhow!("not edited"))?;

        let selection = selection.apply_delta(&delta, true, InsertDrift::Default);

        let mut transformer = Transformer::new(&delta);
        let offset = transformer.transform(start_offset, false);
        let snippet_tabs = snippet.tabs(offset);

        let doc = self.doc();
        if snippet_tabs.is_empty() {
            doc.buffer.update(|buffer| {
                cursor.update_selection(buffer, selection);
                buffer.set_cursor_before(old_cursor);
                buffer.set_cursor_after(cursor.mode.clone());
            });
            self.cursor().set(cursor);
            self.apply_deltas(&[(b_text, delta, inval_lines)]);
            return Ok(());
        }

        let mut selection = lapce_core::selection::Selection::new();
        let (_tab, (start, end)) = &snippet_tabs[0];
        let region = lapce_core::selection::SelRegion::new(*start, *end, None);
        selection.add_region(region);
        cursor.set_insert(selection);

        doc.buffer.update(|buffer| {
            buffer.set_cursor_before(old_cursor);
            buffer.set_cursor_after(cursor.mode.clone());
        });
        self.cursor().set(cursor);
        self.apply_deltas(&[(b_text, delta, inval_lines)]);
        self.add_snippet_placeholders(snippet_tabs);
        Ok(())
    }

    fn add_snippet_placeholders(
        &self,
        new_placeholders: Vec<(usize, (usize, usize))>,
    ) {
        self.snippet.update(|snippet| {
            if snippet.is_none() {
                if new_placeholders.len() > 1 {
                    *snippet = Some(new_placeholders);
                }
                return;
            }

            let Some(placeholders) = snippet.as_mut() else {
                return;
            };

            let mut current = 0;
            let offset = self.cursor().get_untracked().offset();
            for (i, (_, (start, end))) in placeholders.iter().enumerate() {
                if *start <= offset && offset <= *end {
                    current = i;
                    break;
                }
            }

            let v = placeholders.split_off(current);
            placeholders.extend_from_slice(&new_placeholders);
            placeholders.extend_from_slice(&v[1..]);
        });
    }
}
