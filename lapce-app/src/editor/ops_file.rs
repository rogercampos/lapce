//! File-level operations on [`EditorData`]: save (with optional
//! format-on-save), standalone format, persisting the cursor/scroll offset
//! to the DB on tab close, and the prepare-rename LSP flow.

use std::sync::Arc;

use floem::{
    ext_event::create_ext_action,
    reactive::{SignalGet, SignalWith, use_context},
};
use lapce_core::{
    buffer::rope_text::RopeText, command::EditCommand,
    rope_text_pos::RopeTextPosition,
};
use lapce_rpc::proxy::ProxyResponse;
use tracing::instrument;

use crate::{
    command::InternalCommand, db::LapceDb, doc::DocContent, editor::EditorData,
};

impl EditorData {
    fn do_save(&self, after_action: impl FnOnce() + 'static) {
        self.doc().save(after_action);
    }

    pub fn save(
        &self,
        allow_formatting: bool,
        after_action: impl FnOnce() + 'static,
    ) {
        let doc = self.doc();
        let is_pristine = doc.is_pristine();
        let content = doc.content.get_untracked();

        if let DocContent::Scratch { .. } = &content {
            self.common
                .internal_command
                .send(InternalCommand::SaveScratchDoc { doc });
            return;
        }

        if content.path().is_some() && is_pristine {
            return;
        }

        let config = self.common.config.get_untracked();
        let DocContent::File { path, .. } = content else {
            return;
        };

        // If we are disallowing formatting (such as due to a manual save without formatting),
        // then we skip normalizing line endings as a common reason for that is large files.
        // (but if the save is typical, even if config format_on_save is false, we normalize)
        if allow_formatting && config.editor.normalize_line_endings {
            self.run_edit_command(&EditCommand::NormalizeLineEndings);
        }

        let rev = doc.rev();
        let format_on_save = allow_formatting && config.editor.format_on_save;
        if format_on_save {
            let editor = self.clone();
            let send = create_ext_action(self.scope, move |result| {
                if let Ok(Ok(ProxyResponse::GetDocumentFormatting { edits })) =
                    result
                {
                    let current_rev = editor.doc().rev();
                    if current_rev == rev {
                        editor.do_text_edit(&edits);
                    }
                }
                editor.do_save(after_action);
            });

            let (tx, rx) = crossbeam_channel::bounded(1);
            let proxy = self.common.proxy.clone();
            std::thread::spawn(move || {
                proxy.get_document_formatting(path, move |result| {
                    if let Err(err) = tx.send(result) {
                        tracing::error!("{:?}", err);
                    }
                });
                let result = rx.recv_timeout(std::time::Duration::from_secs(1));
                send(result);
            });
        } else {
            self.do_save(after_action);
        }
    }

    pub fn format(&self) {
        let doc = self.doc();
        let rev = doc.rev();
        let content = doc.content.get_untracked();

        if let DocContent::File { path, .. } = content {
            let editor = self.clone();
            let send = create_ext_action(self.scope, move |result| {
                if let Ok(Ok(ProxyResponse::GetDocumentFormatting { edits })) =
                    result
                {
                    let current_rev = editor.doc().rev();
                    if current_rev == rev {
                        editor.do_text_edit(&edits);
                    }
                }
            });

            let (tx, rx) = crossbeam_channel::bounded(1);
            let proxy = self.common.proxy.clone();
            std::thread::spawn(move || {
                proxy.get_document_formatting(path, move |result| {
                    if let Err(err) = tx.send(result) {
                        tracing::error!("{:?}", err);
                    }
                });
                let result = rx.recv_timeout(std::time::Duration::from_secs(1));
                send(result);
            });
        }
    }

    pub fn save_doc_position(&self) {
        let doc = self.doc();
        let path = match doc.loaded_file_path() {
            Some(path) => path,
            None => return,
        };

        let cursor_offset = self.cursor().with_untracked(|c| c.offset());
        let scroll_offset = self.viewport().with_untracked(|v| v.origin().to_vec2());

        let db: Arc<LapceDb> = use_context().unwrap();
        db.save_doc_position(
            &self.common.workspace,
            path,
            cursor_offset,
            scroll_offset,
        );
    }

    #[instrument]
    pub(super) fn rename(&self) {
        let doc = self.doc();
        let path = match doc.loaded_file_path() {
            Some(path) => path,
            None => return,
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (position, rev) = doc
            .buffer
            .with_untracked(|buffer| (buffer.offset_to_position(offset), doc.rev()));

        let cursor = self.cursor();
        let buffer = doc.buffer;
        let internal_command = self.common.internal_command;
        let local_path = path.clone();
        let send = create_ext_action(self.scope, move |result| {
            if let Ok(ProxyResponse::PrepareRename { resp }) = result {
                if buffer.with_untracked(|buffer| buffer.rev()) != rev {
                    return;
                }

                if cursor.with_untracked(|c| c.offset()) != offset {
                    return;
                }

                let (start, _end, position, placeholder) =
                    buffer.with_untracked(|buffer| match resp {
                        lsp_types::PrepareRenameResponse::Range(range) => (
                            buffer.offset_of_position(&range.start),
                            buffer.offset_of_position(&range.end),
                            range.start,
                            None,
                        ),
                        lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
                            range,
                            placeholder,
                        } => (
                            buffer.offset_of_position(&range.start),
                            buffer.offset_of_position(&range.end),
                            range.start,
                            Some(placeholder),
                        ),
                        lsp_types::PrepareRenameResponse::DefaultBehavior {
                            ..
                        } => {
                            let start = buffer.prev_code_boundary(offset);
                            let position = buffer.offset_to_position(start);
                            (
                                start,
                                buffer.next_code_boundary(offset),
                                position,
                                None,
                            )
                        }
                    });
                let placeholder = placeholder.unwrap_or_else(|| {
                    buffer.with_untracked(|buffer| {
                        let (start, end) = buffer.select_word(offset);
                        buffer.slice_to_cow(start..end).to_string()
                    })
                });
                internal_command.send(InternalCommand::StartRename {
                    path: local_path.clone(),
                    placeholder,
                    start,
                    position,
                });
            }
        });
        self.common
            .proxy
            .prepare_rename(path, position, move |result| {
                send(result);
            });
    }
}
