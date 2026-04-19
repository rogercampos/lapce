//! Buffer-level navigation operations on [`EditorData`]: jumping to a
//! location (with DB-restored cursor fallback for fresh docs), moving the
//! cursor to a specific position, and centring the viewport on an offset.

use std::sync::Arc;

use floem::{
    kurbo::Vec2,
    reactive::{SignalGet, SignalUpdate, SignalWith, use_context},
};
use lapce_core::{
    buffer::rope_text::RopeText,
    cursor::{Cursor, CursorMode},
    selection::Selection,
};
use lsp_types::TextEdit;

use crate::{
    db::LapceDb,
    editor::{
        EditorData,
        location::{EditorLocation, EditorPosition},
    },
};

impl EditorData {
    fn do_go_to_location(
        &self,
        location: EditorLocation,
        edits: Option<Vec<TextEdit>>,
    ) {
        if let Some(position) = location.position {
            self.go_to_position(position, location.scroll_offset, edits);
        } else if let Some(edits) = edits.as_ref() {
            self.do_text_edit(edits);
        } else {
            let db: Arc<LapceDb> = use_context().unwrap();
            if let Ok(info) = db.get_doc_info(&self.common.workspace, &location.path)
            {
                self.go_to_position(
                    EditorPosition::Offset(info.cursor_offset),
                    Some(Vec2::new(info.scroll_offset.0, info.scroll_offset.1)),
                    edits,
                );
            }
        }
    }

    /// Navigate to a location. When `new_doc` is true, the document hasn't been
    /// loaded from disk yet, so we create a reactive effect that waits for
    /// `loaded` to become true before performing the jump. The effect self-terminates
    /// by returning `true` once executed, preventing repeated navigation on
    /// subsequent signal updates.
    pub fn go_to_location(
        &self,
        location: EditorLocation,
        new_doc: bool,
        edits: Option<Vec<TextEdit>>,
    ) {
        if !new_doc {
            self.do_go_to_location(location, edits);
        } else {
            let loaded = self.doc().loaded;
            let editor = self.clone();
            self.scope.create_effect(move |prev_loaded| {
                if prev_loaded == Some(true) {
                    return true;
                }

                let loaded = loaded.get();
                if loaded {
                    editor.do_go_to_location(location.clone(), edits.clone());
                }
                loaded
            });
        }
    }

    pub fn go_to_position(
        &self,
        position: EditorPosition,
        scroll_offset: Option<Vec2>,
        edits: Option<Vec<TextEdit>>,
    ) {
        let offset = self
            .doc()
            .buffer
            .with_untracked(|buffer| position.to_offset(buffer));
        self.cursor().set(Cursor::new(
            CursorMode::Insert(Selection::caret(offset)),
            None,
            None,
        ));
        if let Some(scroll_offset) = scroll_offset {
            self.editor.scroll_to.set(Some(scroll_offset));
        } else {
            self.center_on_offset(offset);
        }
        if let Some(edits) = edits.as_ref() {
            self.do_text_edit(edits);
        }
    }

    /// Center the viewport on the given buffer offset. If the viewport hasn't
    /// been laid out yet (height == 0), defers the scroll via a reactive effect
    /// that fires once the viewport becomes valid.
    fn center_on_offset(&self, offset: usize) {
        let line = self
            .doc()
            .buffer
            .with_untracked(|buffer| buffer.line_of_offset(offset));
        let line = self.visual_line(line);
        let config = self.common.config.get_untracked();
        let line_height = config.editor.line_height();
        let viewport = self.editor.viewport.get_untracked();
        if viewport.height() > 0.0 {
            let target_y = (line * line_height) as f64;
            let center_y =
                target_y - viewport.height() / 2.0 + line_height as f64 / 2.0;
            self.editor
                .scroll_to
                .set(Some(Vec2::new(viewport.x0, center_y.max(0.0))));
        } else {
            // Viewport not laid out yet (e.g. tab just switched). Defer scroll
            // until the viewport becomes valid.
            let scroll_to = self.editor.scroll_to;
            let viewport_signal = self.editor.viewport;
            self.scope.create_effect(move |done| {
                if done == Some(true) {
                    return true;
                }
                let vp = viewport_signal.get();
                if vp.height() > 0.0 {
                    let target_y = (line * line_height) as f64;
                    let center_y =
                        target_y - vp.height() / 2.0 + line_height as f64 / 2.0;
                    scroll_to.set(Some(Vec2::new(vp.x0, center_y.max(0.0))));
                    return true;
                }
                false
            });
        }
    }
}
