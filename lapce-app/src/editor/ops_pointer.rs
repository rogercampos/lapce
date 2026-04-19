//! Pointer/mouse event handling and hover popups on [`EditorData`].
//!
//! Extracted from `editor.rs` as a separate `impl EditorData` block. Covers
//! the full pointer lifecycle (`pointer_down`/`move`/`up`/`leave`), click
//! dispatch (`left_click` + N-click variants, `right_click`), the inlay-hint
//! ray-cast used by Cmd+click go-to-definition (`find_hint`), and the
//! diagnostic/LSP hover popups (`update_diagnostic_hover`, `update_hover`).
//! `update_hover` is `pub(super)` because `editor.rs::run_focus_command`
//! still dispatches to it; every other helper stays module-private.

use floem::{
    action::{TimerToken, show_context_menu},
    ext_event::create_ext_action,
    kurbo::Point,
    menu::{Menu, MenuItem},
    pointer::{MouseButton, PointerInputEvent, PointerMoveEvent},
    reactive::{SignalGet, SignalUpdate, SignalWith},
};
use itertools::Itertools;
use lapce_core::{
    buffer::rope_text::RopeText,
    command::{EditCommand, FocusCommand},
    language::LapceLanguage,
    rope_text_pos::RopeTextPosition,
};
use lapce_rpc::proxy::ProxyResponse;
use lsp_types::{DiagnosticSeverity, GotoDefinitionResponse};
use tracing::instrument;

use crate::{
    command::{CommandKind, InternalCommand, LapceCommand, LapceWorkbenchCommand},
    config::layout::LapceLayout,
    doc::DocContent,
    editor::{
        EditorData, FindHintRs, find_hint,
        location::{EditorLocation, EditorPosition},
        parse_hover_resp,
        ruby::{is_ruby_type_file, ruby_word_start},
    },
    markdown::parse_markdown,
    workspace_data::Focus,
};

impl EditorData {
    pub fn pointer_down(&self, pointer_event: &PointerInputEvent) {
        self.cancel_completion();
        self.cancel_inline_completion();
        if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
            self.common
                .internal_command
                .send(InternalCommand::FocusEditorTab { editor_tab_id });
        }
        if self.kind.get_untracked().is_normal()
            && self
                .doc()
                .content
                .with_untracked(|content| !content.is_local())
        {
            self.common.focus.set(Focus::Workbench);
            self.find_state.find_focus.set(false);
        }
        match pointer_event.button.mouse_button() {
            MouseButton::Primary => {
                self.active().set(true);
                self.left_click(pointer_event);

                let y =
                    pointer_event.pos.y - self.editor.viewport.get_untracked().y0;
                if self.sticky_header_height.get_untracked() > y {
                    let index = y as usize
                        / self.common.config.get_untracked().editor.line_height();
                    if let (Some(path), Some(line)) = (
                        self.doc().content.get_untracked().path(),
                        self.sticky_header_info
                            .get_untracked()
                            .sticky_lines
                            .get(index),
                    ) {
                        self.common.internal_command.send(
                            InternalCommand::JumpToLocation {
                                location: EditorLocation {
                                    path: path.clone(),
                                    position: Some(EditorPosition::Line(*line)),
                                    scroll_offset: None,

                                    same_editor_tab: false,
                                },
                            },
                        );
                        return;
                    }
                }

                if (cfg!(target_os = "macos") && pointer_event.modifiers.meta())
                    || (cfg!(not(target_os = "macos"))
                        && pointer_event.modifiers.control())
                {
                    let rs = self.find_hint(pointer_event.pos);
                    match rs {
                        FindHintRs::NoMatchBreak
                        | FindHintRs::NoMatchContinue { .. } => {
                            self.common.lapce_command.send(LapceCommand {
                                kind: CommandKind::Focus(
                                    FocusCommand::GotoDefinition,
                                ),
                                data: None,
                            })
                        }
                        FindHintRs::MatchWithoutLocation => {}
                        FindHintRs::Match(location) => {
                            let Ok(path) = location.uri.to_file_path() else {
                                return;
                            };
                            self.common.internal_command.send(
                                InternalCommand::JumpToLocation {
                                    location: EditorLocation {
                                        path,
                                        position: Some(EditorPosition::Position(
                                            location.range.start,
                                        )),
                                        scroll_offset: None,

                                        same_editor_tab: false,
                                    },
                                },
                            );
                        }
                    }
                }
            }
            MouseButton::Secondary => {
                self.right_click(pointer_event);
            }
            _ => {}
        }
    }

    fn find_hint(&self, pos: Point) -> FindHintRs {
        let rs = self.editor.line_col_of_point_with_phantom(pos);
        let line = rs.0 as u32;
        let index = rs.1 as u32;
        self.doc().inlay_hints.with_untracked(|x| {
            if let Some(hints) = x {
                let mut pre_len = 0;
                for hint in hints
                    .iter()
                    .filter_map(|(_, hint)| {
                        if hint.position.line == line {
                            Some(hint)
                        } else {
                            None
                        }
                    })
                    .sorted_by(|pre, next| {
                        pre.position.character.cmp(&next.position.character)
                    })
                {
                    match find_hint(pre_len, index, hint) {
                        FindHintRs::NoMatchContinue { pre_hint_len } => {
                            pre_len = pre_hint_len;
                        }
                        rs => return rs,
                    }
                }
                FindHintRs::NoMatchBreak
            } else {
                FindHintRs::NoMatchBreak
            }
        })
    }

    #[instrument]
    fn left_click(&self, pointer_event: &PointerInputEvent) {
        match pointer_event.count {
            1 => {
                self.single_click(pointer_event);
            }
            2 => {
                self.double_click(pointer_event);
            }
            3 => {
                self.triple_click(pointer_event);
            }
            _ => {}
        }
    }

    #[instrument]
    fn single_click(&self, pointer_event: &PointerInputEvent) {
        self.editor.single_click(pointer_event);
    }

    #[instrument]
    fn double_click(&self, pointer_event: &PointerInputEvent) {
        self.editor.double_click(pointer_event);
    }

    #[instrument]
    fn triple_click(&self, pointer_event: &PointerInputEvent) {
        self.editor.triple_click(pointer_event);
    }

    #[instrument]
    pub fn pointer_move(&self, pointer_event: &PointerMoveEvent) {
        let mode = self.cursor().with_untracked(|c| c.get_mode());
        let (offset, is_inside) =
            self.editor.offset_of_point(mode, pointer_event.pos);
        if self.active().get_untracked()
            && self.cursor().with_untracked(|c| c.offset()) != offset
        {
            self.editor
                .extend_drag_selection(offset, pointer_event.modifiers.alt());
        }

        // Cmd+hover definition link styling
        let is_cmd = (cfg!(target_os = "macos") && pointer_event.modifiers.meta())
            || (cfg!(not(target_os = "macos")) && pointer_event.modifiers.control());

        // Pixel-level moves within the same character repeat the same hover
        // work (diagnostic span scan + Cmd-link boundary lookup + LSP probe).
        // Skip all of it when nothing that influences the outcome has changed.
        let hover_state = (offset, is_inside, is_cmd);
        if self.hover.state.get_untracked() == Some(hover_state) {
            return;
        }
        self.hover.state.set(Some(hover_state));

        self.update_diagnostic_hover(offset);

        if is_cmd && is_inside {
            if let Some(path) = self.doc().loaded_file_path() {
                let doc = self.doc();
                let language = doc.syntax.with_untracked(|s| s.language);
                let (start_offset, end_offset) =
                    doc.buffer.with_untracked(|buffer| {
                        let mut start = buffer.prev_code_boundary(offset);
                        if language == LapceLanguage::Ruby {
                            start = ruby_word_start(buffer, start);
                        }
                        (start, buffer.next_code_boundary(offset))
                    });

                if start_offset < end_offset
                    && self.hover.link_range.get_untracked()
                        != Some((start_offset, end_offset))
                {
                    self.hover.link_range.set(None);
                    doc.clear_text_cache();

                    let link_hover_range = self.hover.link_range;
                    let cache_rev = doc.cache_rev;
                    let send = create_ext_action(
                        self.scope,
                        move |has_definition: bool| {
                            if has_definition {
                                link_hover_range
                                    .set(Some((start_offset, end_offset)));
                                cache_rev.try_update(|r| *r += 1);
                            }
                        },
                    );

                    let position =
                        doc.buffer.with_untracked(|b| b.offset_to_position(offset));
                    self.common.proxy.get_definition(
                        start_offset,
                        path,
                        position,
                        move |result| {
                            if let Ok(ProxyResponse::GetDefinitionResponse {
                                definition,
                                ..
                            }) = result
                            {
                                let has_def = match definition {
                                    GotoDefinitionResponse::Scalar(loc) => {
                                        !is_ruby_type_file(&loc.uri)
                                    }
                                    GotoDefinitionResponse::Array(locs) => locs
                                        .iter()
                                        .any(|l| !is_ruby_type_file(&l.uri)),
                                    GotoDefinitionResponse::Link(links) => links
                                        .iter()
                                        .any(|l| !is_ruby_type_file(&l.target_uri)),
                                };
                                send(has_def);
                            }
                        },
                    );
                }
            }
        } else if self.hover.link_range.get_untracked().is_some() {
            self.hover.link_range.set(None);
            self.doc().clear_text_cache();
        }
    }

    #[instrument]
    pub fn pointer_up(&self, pointer_event: &PointerInputEvent) {
        self.editor.pointer_up(pointer_event);
    }

    #[instrument]
    pub fn pointer_leave(&self) {
        self.hover.timer.set(TimerToken::INVALID);
        self.hover.state.set(None);
        if self.hover.link_range.get_untracked().is_some() {
            self.hover.link_range.set(None);
            self.doc().clear_text_cache();
        }
    }

    #[instrument]
    fn right_click(&self, pointer_event: &PointerInputEvent) {
        let mode = self.cursor().with_untracked(|c| c.get_mode());
        let (offset, _) = self.editor.offset_of_point(mode, pointer_event.pos);
        let doc = self.doc();
        let pointer_inside_selection = doc.buffer.with_untracked(|buffer| {
            self.cursor()
                .with_untracked(|c| c.edit_selection(buffer).contains(offset))
        });
        if !pointer_inside_selection {
            // move cursor to pointer position if outside current selection
            self.single_click(pointer_event);
        }

        let (path, is_file) = doc.content.with_untracked(|content| match content {
            DocContent::File { path, .. } => {
                (Some(path.to_path_buf()), path.is_file())
            }
            DocContent::Local
            | DocContent::History(_)
            | DocContent::Scratch { .. } => (None, false),
        });
        let mut menu = Menu::new("");
        let cmds = if is_file {
            if path
                .as_ref()
                .and_then(|x| x.file_name().and_then(|x| x.to_str()))
                .map(|x| x == "run.toml")
                .unwrap_or_default()
            {
                vec![
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInPanel,
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInFileExplorer,
                    )),
                    None,
                    Some(CommandKind::Edit(EditCommand::ClipboardCut)),
                    Some(CommandKind::Edit(EditCommand::ClipboardCopy)),
                    Some(CommandKind::Edit(EditCommand::ClipboardPaste)),
                ]
            } else {
                vec![
                    Some(CommandKind::Focus(FocusCommand::GotoDefinition)),
                    Some(CommandKind::Focus(FocusCommand::GotoTypeDefinition)),
                    Some(CommandKind::Focus(FocusCommand::Rename)),
                    None,
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInPanel,
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInFileExplorer,
                    )),
                    None,
                    Some(CommandKind::Edit(EditCommand::ClipboardCut)),
                    Some(CommandKind::Edit(EditCommand::ClipboardCopy)),
                    Some(CommandKind::Edit(EditCommand::ClipboardPaste)),
                ]
            }
        } else {
            vec![
                Some(CommandKind::Edit(EditCommand::ClipboardCut)),
                Some(CommandKind::Edit(EditCommand::ClipboardCopy)),
                Some(CommandKind::Edit(EditCommand::ClipboardPaste)),
            ]
        };
        let lapce_command = self.common.lapce_command;
        for cmd in cmds {
            if let Some(cmd) = cmd {
                menu = menu.entry(
                    MenuItem::new(cmd.desc().unwrap_or_else(|| cmd.str())).action(
                        move || {
                            lapce_command.send(LapceCommand {
                                kind: cmd.clone(),
                                data: None,
                            })
                        },
                    ),
                );
            } else {
                menu = menu.separator();
            }
        }
        show_context_menu(menu, None);
    }

    /// Shows a hover popup with diagnostic messages if the given offset falls
    /// within a diagnostic range. Clears the hover otherwise.
    fn update_diagnostic_hover(&self, offset: usize) {
        let doc = self.doc();
        let mut messages: Vec<(DiagnosticSeverity, String)> = Vec::new();
        doc.diagnostics.diagnostics_span.with_untracked(|diags| {
            for (iv, diag) in diags.iter_chunks(0..usize::MAX) {
                if iv.start() <= offset
                    && offset < iv.end()
                    && diag.severity < Some(DiagnosticSeverity::HINT)
                {
                    let severity =
                        diag.severity.unwrap_or(DiagnosticSeverity::WARNING);
                    messages.push((severity, diag.message.clone()));
                }
            }
        });

        if messages.is_empty() {
            self.common.hover.active.set(false);
            return;
        }

        let config = self.common.config.get_untracked();
        let text = messages
            .iter()
            .map(|(sev, msg)| {
                let prefix = if *sev == DiagnosticSeverity::ERROR {
                    "Error"
                } else if *sev == DiagnosticSeverity::WARNING {
                    "Warning"
                } else {
                    "Info"
                };
                format!("**{prefix}**: {msg}")
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let content = parse_markdown(&text, LapceLayout::UI_LINE_HEIGHT, &config);
        let hover = &self.common.hover;
        hover.content.set(content);
        hover.offset.set(offset);
        hover.editor_id.set(self.id());
        hover.active.set(true);
    }

    #[instrument]
    pub(super) fn update_hover(&self, offset: usize) {
        let doc = self.doc();
        let path = doc
            .content
            .with_untracked(|content| content.path().cloned());
        let position = doc
            .buffer
            .with_untracked(|buffer| buffer.offset_to_position(offset));
        let path = match path {
            Some(path) => path,
            None => return,
        };
        let config = self.common.config;
        let hover_data = self.common.hover.clone();
        let editor_id = self.id();
        let send = create_ext_action(self.scope, move |resp| {
            if let Ok(ProxyResponse::HoverResponse { hover, .. }) = resp {
                let content = parse_hover_resp(hover, &config.get_untracked());
                hover_data.content.set(content);
                hover_data.offset.set(offset);
                hover_data.editor_id.set(editor_id);
                hover_data.active.set(true);
            }
        });
        self.common.proxy.get_hover(0, path, position, |resp| {
            send(resp);
        });
    }
}
