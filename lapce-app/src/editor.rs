use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    str::FromStr,
    sync::Arc,
};

use floem::{
    action::{TimerToken, show_context_menu},
    ext_event::create_ext_action,
    keyboard::Modifiers,
    kurbo::{Point, Rect, Vec2},
    menu::{Menu, MenuItem},
    pointer::{MouseButton, PointerInputEvent, PointerMoveEvent},
    prelude::SignalTrack,
    reactive::{
        ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith, batch,
        use_context,
    },
    views::editor::{
        Editor,
        command::CommandExecuted,
        id::EditorId,
        movement,
        text::Document,
        view::{LineInfo, ScreenLines, ScreenLinesBase},
        visual_line::{ConfigId, Lines, TextLayoutProvider, VLine},
    },
};
use itertools::Itertools;
use lapce_core::{
    buffer::{
        Buffer, InvalLines,
        rope_text::{RopeText, RopeTextVal},
    },
    command::{EditCommand, FocusCommand, ScrollCommand},
    cursor::{Cursor, CursorMode},
    editor::EditType,
    language::LapceLanguage,
    rope_text_pos::RopeTextPosition,
    selection::{InsertDrift, SelRegion, Selection},
};
use lapce_rpc::{buffer::BufferId, plugin::PluginId, proxy::ProxyResponse};
use lapce_xi_rope::{Rope, RopeDelta, Transformer};
use lsp_types::{
    CodeActionResponse, CompletionItem, CompletionTextEdit, DiagnosticSeverity,
    GotoDefinitionResponse, HoverContents, InlayHint, InlayHintLabel,
    InlineCompletionTriggerKind, Location, MarkedString, MarkupKind, TextEdit,
};
use serde::{Deserialize, Serialize};
use view::StickyHeaderInfo;

use self::location::{EditorLocation, EditorPosition};
use crate::{
    command::{CommandKind, InternalCommand, LapceCommand, LapceWorkbenchCommand},
    completion::CompletionStatus,
    config::{LapceConfig, layout::LapceLayout},
    db::LapceDb,
    doc::{Doc, DocContent},
    editor_tab::EditorTabChild,
    find::{Find, FindProgress, FindResult},
    id::EditorTabId,
    inline_completion::{InlineCompletionItem, InlineCompletionStatus},
    keypress::{KeyPressFocus, condition::Condition},
    lsp::path_from_url,
    main_split::{Editors, MainSplitData, SplitDirection, SplitMoveDirection},
    markdown::{
        MarkdownContent, from_marked_string, from_plaintext, parse_markdown,
    },
    snippet::Snippet,
    tracing::*,
    workspace_data::{CommonData, Focus, WorkspaceData},
};

pub mod gutter;
pub mod location;
pub mod view;

#[derive(Clone, Debug)]
pub enum InlineFindDirection {
    Left,
    Right,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EditorInfo {
    pub content: DocContent,
    pub unsaved: Option<String>,
    pub offset: usize,
    pub scroll_offset: (f64, f64),
}

impl EditorInfo {
    pub fn to_data(
        &self,
        data: MainSplitData,
        editor_tab_id: EditorTabId,
    ) -> EditorId {
        let editors = &data.editors;
        let common = data.common.clone();
        match &self.content {
            DocContent::File { path, .. } => {
                let (doc, new_doc) =
                    data.get_doc(path.clone(), self.unsaved.clone());
                let editor = editors.make_from_doc(
                    data.scope,
                    doc,
                    Some(editor_tab_id),
                    common,
                );
                editor.go_to_location(
                    EditorLocation {
                        path: path.clone(),
                        position: Some(EditorPosition::Offset(self.offset)),
                        scroll_offset: Some(Vec2::new(
                            self.scroll_offset.0,
                            self.scroll_offset.1,
                        )),

                        same_editor_tab: false,
                    },
                    new_doc,
                    None,
                );

                editor.id()
            }
            DocContent::Local => editors.new_local(data.scope, common),
            DocContent::History(_) => editors.new_local(data.scope, common),
            DocContent::Scratch { name, .. } => {
                let doc = data
                    .scratch_docs
                    .try_update(|scratch_docs| {
                        if let Some(doc) = scratch_docs.get(name) {
                            return doc.clone();
                        }
                        let content = DocContent::Scratch {
                            id: BufferId::next(),
                            name: name.to_string(),
                        };
                        let doc = Doc::new_content(
                            data.scope,
                            content,
                            data.editors,
                            data.common.clone(),
                        );
                        let doc = Rc::new(doc);
                        if let Some(unsaved) = &self.unsaved {
                            doc.reload(Rope::from(unsaved), false);
                        }
                        scratch_docs.insert(name.to_string(), doc.clone());
                        doc
                    })
                    .unwrap();

                editors.new_from_doc(data.scope, doc, Some(editor_tab_id), common)
            }
        }
    }
}

#[derive(Clone)]
pub enum EditorViewKind {
    Normal,
    Preview,
}

impl EditorViewKind {
    pub fn is_normal(&self) -> bool {
        matches!(self, EditorViewKind::Normal)
    }
}

#[derive(Clone)]
pub struct OnScreenFind {
    pub active: bool,
    pub pattern: String,
    pub regions: Vec<SelRegion>,
}

pub type SnippetIndex = Vec<(usize, (usize, usize))>;

/// The primary data structure for a single editor instance. Wraps floem's `Editor`
/// with Lapce-specific state: snippet tracking, inline/on-screen find, sticky headers,
/// and the connection to the shared `CommonData` (completion, hover, proxy, etc.).
///
/// `EditorData` is cheaply cloneable (all fields are signals or Rc) -- cloned instances
/// share the same underlying reactive state. The `editor: Rc<Editor>` holds the floem
/// editor which owns the cursor, viewport, text layout cache, and document reference.
#[derive(Clone)]
pub struct EditorData {
    pub scope: Scope,
    /// Which editor tab contains this editor. `None` for preview/local editors
    /// (e.g., search modal preview, palette preview). When `None`, focus-related
    /// commands like SplitVertical and FocusEditorTab are no-ops.
    pub editor_tab_id: RwSignal<Option<EditorTabId>>,
    /// Active snippet placeholder positions, sorted by tab-stop index.
    /// Each entry is (tab_stop_index, (start_offset, end_offset)).
    /// Set to `None` when cursor moves outside all placeholder regions.
    pub snippet: RwSignal<Option<SnippetIndex>>,
    pub inline_find: RwSignal<Option<InlineFindDirection>>,
    pub on_screen_find: RwSignal<OnScreenFind>,
    pub last_inline_find: RwSignal<Option<(InlineFindDirection, String)>>,
    /// Whether the find/replace bar has keyboard focus (as opposed to the editor body).
    /// When true, typed characters are routed to the find/replace editors instead.
    pub find_focus: RwSignal<bool>,
    pub find: Find,
    pub find_result: FindResult,
    pub find_editor_signal: RwSignal<Option<EditorData>>,
    pub replace_editor_signal: RwSignal<Option<EditorData>>,
    pub editor: Rc<Editor>,
    /// Distinguishes normal (workbench) editors from preview editors. Preview editors
    /// skip sticky headers and don't steal focus on pointer_down.
    pub kind: RwSignal<EditorViewKind>,
    pub sticky_header_height: RwSignal<f64>,
    pub mouse_hover_timer: RwSignal<TimerToken>,
    /// Range (start_offset, end_offset) of the symbol with a confirmed definition link.
    /// Set when Cmd is held and the LSP confirms a definition exists.
    pub link_hover_range: RwSignal<Option<(usize, usize)>>,
    pub common: Rc<CommonData>,
    pub sticky_header_info: RwSignal<StickyHeaderInfo>,
}

impl std::fmt::Debug for EditorData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorData")
            .field("id", &self.id())
            .finish()
    }
}

impl PartialEq for EditorData {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl EditorData {
    fn new(
        cx: Scope,
        editor: Editor,
        editor_tab_id: Option<EditorTabId>,
        common: Rc<CommonData>,
    ) -> Self {
        let cx = cx.create_child();

        EditorData {
            scope: cx,
            editor_tab_id: cx.create_rw_signal(editor_tab_id),
            snippet: cx.create_rw_signal(None),
            inline_find: cx.create_rw_signal(None),
            on_screen_find: cx.create_rw_signal(OnScreenFind {
                active: false,
                pattern: "".to_string(),
                regions: Vec::new(),
            }),
            last_inline_find: cx.create_rw_signal(None),
            find_focus: cx.create_rw_signal(false),
            find: Find::new(cx),
            find_result: FindResult::new(cx),
            find_editor_signal: cx.create_rw_signal(None),
            replace_editor_signal: cx.create_rw_signal(None),
            editor: Rc::new(editor),
            kind: cx.create_rw_signal(EditorViewKind::Normal),
            sticky_header_height: cx.create_rw_signal(0.0),
            mouse_hover_timer: cx.create_rw_signal(TimerToken::INVALID),
            link_hover_range: cx.create_rw_signal(None),
            common,
            sticky_header_info: cx.create_rw_signal(StickyHeaderInfo::default()),
        }
    }

    /// Create a new local editor.  
    /// You should prefer calling [`Editors::make_local`] / [`Editors::new_local`] instead to
    /// register the editor.
    pub fn new_local(cx: Scope, editors: Editors, common: Rc<CommonData>) -> Self {
        Self::new_local_id(cx, EditorId::next(), editors, common)
    }

    /// Create a new local editor with the given id.  
    /// You should prefer calling [`Editors::make_local`] / [`Editors::new_local`] instead to
    /// register the editor.
    pub fn new_local_id(
        cx: Scope,
        editor_id: EditorId,
        editors: Editors,
        common: Rc<CommonData>,
    ) -> Self {
        let cx = cx.create_child();
        let doc = Rc::new(Doc::new_local(cx, editors, common.clone()));
        let editor = doc.create_editor(cx, editor_id);
        Self::new(cx, editor, None, common)
    }

    /// Create a new editor with a specific doc.  
    /// You should prefer calling [`Editors::new_editor_doc`] / [`Editors::make_from_doc`] instead.
    pub fn new_doc(
        cx: Scope,
        doc: Rc<Doc>,
        editor_tab_id: Option<EditorTabId>,
        common: Rc<CommonData>,
    ) -> Self {
        let editor = doc.create_editor(cx, EditorId::next());
        Self::new(cx, editor, editor_tab_id, common)
    }

    /// Swap out the document this editor is for
    pub fn update_doc(&self, doc: Rc<Doc>) {
        let style = doc.styling();
        self.editor.update_doc(doc, Some(style));
    }

    /// Create a new editor using the same underlying [`Doc`]  
    pub fn copy(&self, cx: Scope, editor_tab_id: Option<EditorTabId>) -> Self {
        let cx = cx.create_child();

        let editor =
            Self::new_doc(cx, self.doc(), editor_tab_id, self.common.clone());
        editor.editor.cursor.set(self.editor.cursor.get_untracked());
        editor
            .editor
            .viewport
            .set(self.editor.viewport.get_untracked());
        editor.editor.scroll_to.set(Some(
            self.editor.viewport.get_untracked().origin().to_vec2(),
        ));
        editor
            .editor
            .last_movement
            .set(self.editor.last_movement.get_untracked());

        editor
    }

    pub fn id(&self) -> EditorId {
        self.editor.id()
    }

    pub fn editor_info(&self, _data: &WorkspaceData) -> EditorInfo {
        let offset = self.cursor().get_untracked().offset();
        let scroll_offset = self.viewport().get_untracked().origin();
        let doc = self.doc();
        let is_pristine = doc.is_pristine();
        let unsaved = if is_pristine {
            None
        } else {
            Some(doc.buffer.with_untracked(|b| b.to_string()))
        };
        EditorInfo {
            content: self.doc().content.get_untracked(),
            unsaved,
            offset,
            scroll_offset: (scroll_offset.x, scroll_offset.y),
        }
    }

    pub fn cursor(&self) -> RwSignal<Cursor> {
        self.editor.cursor
    }

    pub fn viewport(&self) -> RwSignal<Rect> {
        self.editor.viewport
    }

    pub fn window_origin(&self) -> RwSignal<Point> {
        self.editor.window_origin
    }

    pub fn scroll_delta(&self) -> RwSignal<Vec2> {
        self.editor.scroll_delta
    }

    pub fn scroll_to(&self) -> RwSignal<Option<Vec2>> {
        self.editor.scroll_to
    }

    pub fn active(&self) -> RwSignal<bool> {
        self.editor.active
    }

    /// Get the line information for lines on the screen.  
    pub fn screen_lines(&self) -> RwSignal<ScreenLines> {
        self.editor.screen_lines
    }

    pub fn doc(&self) -> Rc<Doc> {
        let doc = self.editor.doc();
        (doc as Rc<dyn ::std::any::Any>)
            .downcast::<Doc>()
            .expect("EditorData doc must always be Rc<Doc>")
    }

    /// The signal for the editor's document.  
    pub fn doc_signal(&self) -> DocSignal {
        DocSignal {
            inner: self.editor.doc_signal(),
        }
    }

    pub fn text(&self) -> Rope {
        self.editor.text()
    }

    pub fn rope_text(&self) -> RopeTextVal {
        self.editor.rope_text()
    }

    /// Execute an edit command (insert, delete, etc.) and manage the side effects:
    /// completion triggers, inline completion updates, snippet invalidation.
    /// The `doc_before_edit` snapshot is needed by `show_completion` to inspect
    /// whether the deleted text was whitespace (which suppresses completion).
    fn run_edit_command(&self, cmd: &EditCommand) -> CommandExecuted {
        let doc = self.doc();
        let text = self.editor.rope_text();
        let modal = false;
        let smart_tab = self
            .common
            .config
            .with_untracked(|config| config.editor.smart_tab);
        let doc_before_edit = text.text().clone();
        let mut cursor = self.editor.cursor.get_untracked();
        let mut register = self.common.register.get_untracked();

        let deltas =
            batch(|| doc.do_edit(&mut cursor, cmd, modal, &mut register, smart_tab));

        self.editor.cursor.set(cursor);
        self.editor.register.set(register);

        if show_completion(cmd, &doc_before_edit, &deltas) {
            self.update_completion(false);
        } else {
            self.cancel_completion();
        }

        if *cmd == EditCommand::InsertNewLine {
            // Cancel so that there's no flickering
            self.cancel_inline_completion();
            self.update_inline_completion(InlineCompletionTriggerKind::Automatic);
            self.quit_on_screen_find();
        } else if show_inline_completion(cmd) {
            self.update_inline_completion(InlineCompletionTriggerKind::Automatic);
        } else {
            self.cancel_inline_completion();
        }

        self.apply_deltas(&deltas);
        if let EditCommand::NormalMode = cmd {
            self.snippet.set(None);
            self.quit_on_screen_find();
        }

        CommandExecuted::Yes
    }

    /// Execute a cursor movement command. Saves jump locations for go-back navigation
    /// when the movement is a "jump" (e.g., goto line, goto definition) and differs
    /// from the previous movement -- this avoids flooding the jump history with
    /// consecutive identical movements like repeated arrow presses.
    fn run_move_command(
        &self,
        movement: &lapce_core::movement::Movement,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        self.common.hover.active.set(false);
        if movement.is_jump()
            && movement != &self.editor.last_movement.get_untracked()
        {
            let path = self
                .doc()
                .content
                .with_untracked(|content| content.path().cloned());
            if let Some(path) = path {
                let offset = self.cursor().with_untracked(|c| c.offset());
                let scroll_offset =
                    self.viewport().get_untracked().origin().to_vec2();
                self.common.internal_command.send(
                    InternalCommand::SaveJumpLocation {
                        path,
                        offset,
                        scroll_offset,
                    },
                );
            }
        }
        self.editor.last_movement.set(movement.clone());

        let mut cursor = self.cursor().get_untracked();
        self.common.register.update(|register| {
            movement::move_cursor(
                &self.editor,
                &*self.doc(),
                &mut cursor,
                movement,
                count.unwrap_or(1),
                mods.shift(),
                register,
            )
        });

        self.editor.cursor.set(cursor);

        // After a move, check if the cursor is still within any snippet placeholder
        // region. If not, exit snippet mode entirely. This ensures that navigating
        // away from a snippet (e.g., pressing an arrow key past all placeholders)
        // properly cleans up the snippet state.
        if self.snippet.with_untracked(|s| s.is_some()) {
            self.snippet.update(|snippet| {
                let offset = self.editor.cursor.get_untracked().offset();
                let mut within_region = false;
                if let Some(placeholders) = snippet.as_mut() {
                    for (_, (start, end)) in placeholders {
                        if offset >= *start && offset <= *end {
                            within_region = true;
                            break;
                        }
                    }
                }
                if !within_region {
                    *snippet = None;
                }
            })
        }
        self.cancel_completion();
        CommandExecuted::Yes
    }

    pub fn run_scroll_command(
        &self,
        cmd: &ScrollCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        let prev_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        match cmd {
            ScrollCommand::PageUp => {
                self.editor.page_move(false, mods);
            }
            ScrollCommand::PageDown => {
                self.editor.page_move(true, mods);
            }
            ScrollCommand::ScrollUp => {
                self.scroll(false, count.unwrap_or(1), mods);
            }
            ScrollCommand::ScrollDown => {
                self.scroll(true, count.unwrap_or(1), mods);
            }
            // TODO:
            ScrollCommand::CenterOfWindow => {}
            ScrollCommand::TopOfWindow => {}
            ScrollCommand::BottomOfWindow => {}
        }

        let current_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        if prev_completion_index != current_completion_index {
            self.common.completion.with_untracked(|c| {
                let cursor_offset = self.cursor().with_untracked(|c| c.offset());
                c.update_document_completion(self, cursor_offset);
            });
        }

        CommandExecuted::Yes
    }

    pub fn run_focus_command(
        &self,
        cmd: &FocusCommand,
        _count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        // TODO(minor): Evaluate whether we should split this into subenums,
        // such as actions specific to the actual editor pane, movement, and list movement.
        let prev_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        match cmd {
            FocusCommand::ModalClose => {
                self.cancel_completion();
            }
            FocusCommand::SplitVertical
            | FocusCommand::SplitHorizontal
            | FocusCommand::SplitRight
            | FocusCommand::SplitLeft
            | FocusCommand::SplitUp
            | FocusCommand::SplitDown
            | FocusCommand::SplitExchange => {
                let Some(editor_tab_id) = self.editor_tab_id.get_untracked() else {
                    return CommandExecuted::No;
                };
                let cmd = match cmd {
                    FocusCommand::SplitVertical => InternalCommand::Split {
                        direction: SplitDirection::Vertical,
                        editor_tab_id,
                    },
                    FocusCommand::SplitHorizontal => InternalCommand::Split {
                        direction: SplitDirection::Horizontal,
                        editor_tab_id,
                    },
                    FocusCommand::SplitRight => InternalCommand::SplitMove {
                        direction: SplitMoveDirection::Right,
                        editor_tab_id,
                    },
                    FocusCommand::SplitLeft => InternalCommand::SplitMove {
                        direction: SplitMoveDirection::Left,
                        editor_tab_id,
                    },
                    FocusCommand::SplitUp => InternalCommand::SplitMove {
                        direction: SplitMoveDirection::Up,
                        editor_tab_id,
                    },
                    FocusCommand::SplitDown => InternalCommand::SplitMove {
                        direction: SplitMoveDirection::Down,
                        editor_tab_id,
                    },
                    FocusCommand::SplitExchange => {
                        InternalCommand::SplitExchange { editor_tab_id }
                    }
                    _ => unreachable!(),
                };
                self.common.internal_command.send(cmd);
            }
            FocusCommand::SplitClose => {
                if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
                    self.common.internal_command.send(
                        InternalCommand::EditorTabChildClose {
                            editor_tab_id,
                            child: EditorTabChild::Editor(self.id()),
                        },
                    );
                } else {
                    return CommandExecuted::No;
                }
            }
            FocusCommand::ListNext => {
                self.common.completion.update(|c| {
                    c.next();
                });
            }
            FocusCommand::ListPrevious => {
                self.common.completion.update(|c| {
                    c.previous();
                });
            }
            FocusCommand::ListNextPage => {
                self.common.completion.update(|c| {
                    c.next_page();
                });
            }
            FocusCommand::ListPreviousPage => {
                self.common.completion.update(|c| {
                    c.previous_page();
                });
            }
            FocusCommand::ListSelect => {
                self.select_completion();
                self.cancel_inline_completion();
            }
            FocusCommand::JumpToNextSnippetPlaceholder => {
                self.snippet.update(|snippet| {
                    if let Some(snippet_mut) = snippet.as_mut() {
                        let mut current = 0;
                        let offset = self.cursor().get_untracked().offset();
                        for (i, (_, (start, end))) in snippet_mut.iter().enumerate()
                        {
                            if *start <= offset && offset <= *end {
                                current = i;
                                break;
                            }
                        }

                        let last_placeholder = current + 1 >= snippet_mut.len() - 1;

                        if let Some((_, (start, end))) = snippet_mut.get(current + 1)
                        {
                            let mut selection =
                                lapce_core::selection::Selection::new();
                            let region = lapce_core::selection::SelRegion::new(
                                *start, *end, None,
                            );
                            selection.add_region(region);
                            self.cursor().update(|cursor| {
                                cursor.set_insert(selection);
                            });
                        }

                        if last_placeholder {
                            *snippet = None;
                        }
                        // self.update_signature();
                        self.cancel_completion();
                        self.cancel_inline_completion();
                    }
                });
            }
            FocusCommand::JumpToPrevSnippetPlaceholder => {
                self.snippet.update(|snippet| {
                    if let Some(snippet_mut) = snippet.as_mut() {
                        let mut current = 0;
                        let offset = self.cursor().get_untracked().offset();
                        for (i, (_, (start, end))) in snippet_mut.iter().enumerate()
                        {
                            if *start <= offset && offset <= *end {
                                current = i;
                                break;
                            }
                        }

                        if current > 0 {
                            if let Some((_, (start, end))) =
                                snippet_mut.get(current - 1)
                            {
                                let mut selection =
                                    lapce_core::selection::Selection::new();
                                let region = lapce_core::selection::SelRegion::new(
                                    *start, *end, None,
                                );
                                selection.add_region(region);
                                self.cursor().update(|cursor| {
                                    cursor.set_insert(selection);
                                });
                            }
                            // self.update_signature();
                            self.cancel_completion();
                            self.cancel_inline_completion();
                        }
                    }
                });
            }
            FocusCommand::GotoDefinition => {
                self.go_to_definition();
            }
            FocusCommand::ShowCodeActions => {
                self.show_code_actions(false);
            }
            FocusCommand::SearchWholeWordForward => {
                self.search_whole_word_forward(mods);
            }
            FocusCommand::SearchForward => {
                self.search_forward(mods);
            }
            FocusCommand::SearchBackward => {
                self.search_backward(mods);
            }
            FocusCommand::Save => {
                self.save(true, || {});
            }
            FocusCommand::SaveWithoutFormatting => {
                self.save(false, || {});
            }
            FocusCommand::FormatDocument => {
                self.format();
            }
            FocusCommand::InlineFindLeft => {
                self.inline_find.set(Some(InlineFindDirection::Left));
            }
            FocusCommand::InlineFindRight => {
                self.inline_find.set(Some(InlineFindDirection::Right));
            }
            FocusCommand::OnScreenFind => {
                self.on_screen_find.update(|find| {
                    find.active = true;
                    find.pattern.clear();
                    find.regions.clear();
                });
            }
            FocusCommand::RepeatLastInlineFind => {
                if let Some((direction, c)) = self.last_inline_find.get_untracked() {
                    self.inline_find(direction, &c);
                }
            }
            FocusCommand::Rename => {
                self.rename();
            }
            FocusCommand::ClearSearch => {
                self.clear_search();
            }
            FocusCommand::Search => {
                self.search();
            }
            FocusCommand::SearchAndReplace => {
                self.search_and_replace();
            }
            FocusCommand::ReplaceNext => {
                self.replace_next_and_advance();
            }
            FocusCommand::ReplaceAll => {
                self.replace_all_from_command();
            }
            FocusCommand::FocusFindEditor => {
                self.find.replace_focus.set(false);
            }
            FocusCommand::FocusReplaceEditor => {
                if self.find.replace_active.get_untracked() {
                    self.find.replace_focus.set(true);
                }
            }
            FocusCommand::InlineCompletionSelect => {
                self.select_inline_completion();
            }
            FocusCommand::InlineCompletionNext => {
                self.next_inline_completion();
            }
            FocusCommand::InlineCompletionPrevious => {
                self.previous_inline_completion();
            }
            FocusCommand::InlineCompletionCancel => {
                self.cancel_inline_completion();
            }
            FocusCommand::InlineCompletionInvoke => {
                self.update_inline_completion(InlineCompletionTriggerKind::Invoked);
            }
            FocusCommand::ShowHover => {
                let start_offset = self.doc().buffer.with_untracked(|b| {
                    b.prev_code_boundary(self.cursor().get_untracked().offset())
                });
                self.update_hover(start_offset);
            }
            _ => {}
        }

        let current_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        if prev_completion_index != current_completion_index {
            self.common.completion.with_untracked(|c| {
                let cursor_offset = self.cursor().with_untracked(|c| c.offset());
                c.update_document_completion(self, cursor_offset);
            });
        }

        CommandExecuted::Yes
    }

    /// Jump to the next/previous column on the line which matches the given text
    fn inline_find(&self, direction: InlineFindDirection, c: &str) {
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

    fn quit_on_screen_find(&self) {
        if self.on_screen_find.with_untracked(|s| s.active) {
            self.on_screen_find.update(|f| {
                f.active = false;
                f.pattern.clear();
                f.regions.clear();
            })
        }
    }

    /// Navigate to definition, with a fallback to references.
    /// If the definition resolves to the same position we're already at (i.e., the
    /// cursor is on the definition itself), we fetch references instead, providing
    /// "go to references" behavior when already at the definition site.
    fn go_to_definition(&self) {
        let doc = self.doc();
        let path = match doc.loaded_file_path() {
            Some(path) => path,
            None => return,
        };

        let language = doc.syntax.with_untracked(|s| s.language);
        let offset = self.cursor().with_untracked(|c| c.offset());
        let (start_position, position) = doc.buffer.with_untracked(|buffer| {
            let mut start_offset = buffer.prev_code_boundary(offset);
            if language == LapceLanguage::Ruby {
                start_offset = ruby_word_start(buffer, start_offset);
            }
            let start_position = buffer.offset_to_position(start_offset);
            let position = buffer.offset_to_position(offset);
            (start_position, position)
        });

        enum DefinitionOrReferece {
            Location(EditorLocation),
            Locations(Vec<Location>),
        }

        let internal_command = self.common.internal_command;
        let cursor = self.cursor().read_only();
        let send = create_ext_action(self.scope, move |d| {
            let current_offset = cursor.with_untracked(|c| c.offset());
            if current_offset != offset {
                return;
            }

            match d {
                DefinitionOrReferece::Location(location) => {
                    internal_command
                        .send(InternalCommand::JumpToLocation { location });
                }
                DefinitionOrReferece::Locations(locations) => {
                    internal_command.send(InternalCommand::ShowDefinitionPicker {
                        offset,
                        locations,
                        language,
                    });
                }
            }
        });
        let proxy = self.common.proxy.clone();
        self.common.proxy.get_definition(
            offset,
            path.clone(),
            position,
            move |result| {
                if let Ok(ProxyResponse::GetDefinitionResponse {
                    definition, ..
                }) = result
                {
                    let mut all_locations: Vec<Location> = match definition {
                        GotoDefinitionResponse::Scalar(loc) => vec![loc],
                        GotoDefinitionResponse::Array(locs) => locs,
                        GotoDefinitionResponse::Link(links) => links
                            .into_iter()
                            .map(|link| Location {
                                uri: link.target_uri,
                                range: link.target_selection_range,
                            })
                            .collect(),
                    };
                    {
                        let mut seen = HashSet::new();
                        all_locations.retain(|l| {
                            seen.insert((l.uri.clone(), l.range.start.line))
                        });
                    }
                    if language == LapceLanguage::Ruby {
                        ruby_filter_type_files(&mut all_locations);
                        dedup_ruby_stdlib_gems(&mut all_locations);
                    }

                    if all_locations.is_empty() {
                        return;
                    }

                    // If single result at same position, fall back to references
                    if all_locations.len() == 1
                        && all_locations[0].range.start == start_position
                    {
                        proxy.get_references(
                            path.clone(),
                            position,
                            move |result| {
                                if let Ok(ProxyResponse::GetReferencesResponse {
                                    mut references,
                                }) = result
                                {
                                    {
                                        let mut seen = HashSet::new();
                                        references.retain(|l| {
                                            seen.insert((
                                                l.uri.clone(),
                                                l.range.start.line,
                                            ))
                                        });
                                    }
                                    if language == LapceLanguage::Ruby {
                                        ruby_filter_type_files(&mut references);
                                        dedup_ruby_stdlib_gems(&mut references);
                                    }
                                    if references.is_empty() {
                                        return;
                                    }
                                    if references.len() == 1 {
                                        let location = &references[0];
                                        send(DefinitionOrReferece::Location(
                                            EditorLocation {
                                                path: path_from_url(&location.uri),
                                                position: Some(
                                                    EditorPosition::Position(
                                                        location.range.start,
                                                    ),
                                                ),
                                                scroll_offset: None,
                                                same_editor_tab: false,
                                            },
                                        ));
                                    } else {
                                        send(DefinitionOrReferece::Locations(
                                            references,
                                        ));
                                    }
                                }
                            },
                        );
                    } else if all_locations.len() == 1 {
                        // Single result at different position — jump directly
                        let loc = &all_locations[0];
                        send(DefinitionOrReferece::Location(EditorLocation {
                            path: path_from_url(&loc.uri),
                            position: Some(EditorPosition::Position(
                                loc.range.start,
                            )),
                            scroll_offset: None,
                            same_editor_tab: false,
                        }));
                    } else {
                        // Multiple results — show picker
                        send(DefinitionOrReferece::Locations(all_locations));
                    }
                }
            },
        );
    }

    fn scroll(&self, down: bool, count: usize, mods: Modifiers) {
        self.editor.scroll(
            self.sticky_header_height.get_untracked(),
            down,
            count,
            mods,
        )
    }

    fn select_inline_completion(&self) {
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

    fn next_inline_completion(&self) {
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

    fn previous_inline_completion(&self) {
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
    fn update_inline_completion(&self, trigger_kind: InlineCompletionTriggerKind) {
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
    fn has_inline_completions(&self) -> bool {
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
    fn update_completion(&self, display_if_empty_input: bool) {
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
    fn has_completions(&self) -> bool {
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
    fn apply_deltas(&self, deltas: &[(Rope, RopeDelta, InvalLines)]) {
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
        }
        if let Some(edits) = edits.as_ref() {
            self.do_text_edit(edits);
        }
    }

    pub fn get_code_actions(&self) {
        let doc = self.doc();
        let path = match doc.loaded_file_path() {
            Some(path) => path,
            None => return,
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let exists = doc
            .code_actions()
            .with_untracked(|c| c.contains_key(&offset));

        if exists {
            return;
        }

        // insert some empty data, so that we won't make the request again
        doc.code_actions().update(|c| {
            c.insert(offset, (PluginId(0), im::Vector::new()));
        });

        let (position, rev, diagnostics) = doc.buffer.with_untracked(|buffer| {
            let position = buffer.offset_to_position(offset);
            let rev = doc.rev();

            // Get the diagnostics for the current line, which the LSP might use to inform
            // what code actions are available (such as fixes for the diagnostics).
            let diagnostics = doc
                .diagnostics()
                .diagnostics_span
                .get_untracked()
                .iter_chunks(offset..offset)
                .filter(|(iv, _diag)| iv.start <= offset && iv.end >= offset)
                .map(|(_iv, diag)| diag)
                .cloned()
                .collect();

            (position, rev, diagnostics)
        });

        let send = create_ext_action(
            self.scope,
            move |resp: (PluginId, CodeActionResponse)| {
                if doc.rev() == rev {
                    doc.code_actions().update(|c| {
                        c.insert(offset, (resp.0, resp.1.into()));
                    });
                }
            },
        );

        self.common.proxy.get_code_actions(
            path,
            position,
            diagnostics,
            move |result| {
                if let Ok(ProxyResponse::GetCodeActionsResponse {
                    plugin_id,
                    resp,
                }) = result
                {
                    send((plugin_id, resp))
                }
            },
        );
    }

    pub fn show_code_actions(&self, mouse_click: bool) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let doc = self.doc();
        let code_actions = doc
            .code_actions()
            .with_untracked(|c| c.get(&offset).cloned());
        if let Some((plugin_id, code_actions)) = code_actions {
            if !code_actions.is_empty() {
                self.common.internal_command.send(
                    InternalCommand::ShowCodeActions {
                        offset,
                        mouse_click,
                        plugin_id,
                        code_actions,
                    },
                );
            }
        }
    }

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

    fn search_whole_word_forward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let (word, buffer) = self.doc().buffer.with_untracked(|buffer| {
            let (start, end) = buffer.select_word(offset);
            (buffer.slice_to_cow(start..end).to_string(), buffer.clone())
        });
        if let Some(find_ed) = self.find_editor_signal.get_untracked() {
            find_ed.doc().reload(Rope::from(word.as_str()), true);
            let len = find_ed.doc().buffer.with_untracked(|b| b.len());
            find_ed
                .cursor()
                .update(|c| c.set_insert(Selection::region(0, len)));
        }
        let next = self.find.next(buffer.text(), offset, false, true);

        if let Some((start, _end)) = next {
            self.run_move_command(
                &lapce_core::movement::Movement::Offset(start),
                None,
                mods,
            );
        }
    }

    fn search_forward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let text = self
            .doc()
            .buffer
            .with_untracked(|buffer| buffer.text().clone());
        let next = self.find.next(&text, offset, false, true);

        if let Some((start, _end)) = next {
            self.run_move_command(
                &lapce_core::movement::Movement::Offset(start),
                None,
                mods,
            );
        }
    }

    fn search_backward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let text = self
            .doc()
            .buffer
            .with_untracked(|buffer| buffer.text().clone());
        let next = self.find.next(&text, offset, true, true);

        if let Some((start, _end)) = next {
            self.run_move_command(
                &lapce_core::movement::Movement::Offset(start),
                None,
                mods,
            );
        }
    }

    fn replace_next(&self, text: &str) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let buffer = self.doc().buffer.with_untracked(|buffer| buffer.clone());
        // Use saturating_sub(1) so find.next() includes a match starting exactly
        // at the cursor position (find.next requires start > offset).
        let next =
            self.find
                .next(buffer.text(), offset.saturating_sub(1), false, true);

        if let Some((start, end)) = next {
            let selection = Selection::region(start, end);
            self.do_edit(&selection, &[(selection.clone(), text)]);
            self.find.rev.update(|rev| *rev += 1);
        }
    }

    fn replace_all(&self, text: &str) {
        let offset = self.cursor().with_untracked(|c| c.offset());

        self.update_find();

        let edits: Vec<(Selection, &str)> = self
            .find_result
            .occurrences
            .get_untracked()
            .regions()
            .iter()
            .map(|region| (Selection::region(region.start, region.end), text))
            .collect();
        if !edits.is_empty() {
            self.do_edit(&Selection::caret(offset), &edits);
            self.find.rev.update(|rev| *rev += 1);
        }
    }

    fn replace_next_and_advance(&self) {
        if let Some(replace_ed) = self.replace_editor_signal.get_untracked() {
            let text = replace_ed.doc().buffer.with_untracked(|b| b.to_string());
            self.replace_next(&text);
            self.search_forward(Modifiers::empty());
        }
    }

    fn replace_all_from_command(&self) {
        if let Some(replace_ed) = self.replace_editor_signal.get_untracked() {
            let text = replace_ed.doc().buffer.with_untracked(|b| b.to_string());
            self.replace_all(&text);
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

    fn rename(&self) {
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

    #[instrument]
    pub fn clear_search(&self) {
        self.find.visual.set(false);
        self.find_focus.set(false);
    }

    #[instrument]
    fn search(&self) {
        let pattern = self.word_at_cursor();

        let pattern = if pattern.contains('\n') || pattern.is_empty() {
            None
        } else {
            Some(pattern)
        };

        if let Some(ref p) = pattern {
            if let Some(find_ed) = self.find_editor_signal.get_untracked() {
                find_ed.doc().reload(Rope::from(p.as_str()), true);
                let len = find_ed.doc().buffer.with_untracked(|b| b.len());
                find_ed
                    .cursor()
                    .update(|c| c.set_insert(Selection::region(0, len)));
            }
        }
        self.find.visual.set(true);
        self.find_focus.set(true);
        self.find.replace_active.set(false);
        self.find.replace_focus.set(false);
    }

    #[instrument]
    fn search_and_replace(&self) {
        let pattern = self.word_at_cursor();

        let pattern = if pattern.contains('\n') || pattern.is_empty() {
            None
        } else {
            Some(pattern)
        };

        if let Some(ref p) = pattern {
            if let Some(find_ed) = self.find_editor_signal.get_untracked() {
                find_ed.doc().reload(Rope::from(p.as_str()), true);
                let len = find_ed.doc().buffer.with_untracked(|b| b.len());
                find_ed
                    .cursor()
                    .update(|c| c.set_insert(Selection::region(0, len)));
            }
        }
        self.find.visual.set(true);
        self.find_focus.set(true);
        self.find.replace_active.set(true);
        self.find.replace_focus.set(false);
    }

    /// Execute the find search on the current document's full text.
    /// Called from the paint path to update find results before rendering highlights.
    pub fn update_find(&self) {
        let find_rev = self.find.rev.get_untracked();
        if self.find_result.find_rev.get_untracked() != find_rev {
            if self.find.search_string.with_untracked(|search_string| {
                search_string
                    .as_ref()
                    .map(|s| s.content.is_empty())
                    .unwrap_or(true)
            }) {
                self.find_result.occurrences.set(Selection::new());
            }
            self.find_result.reset();
            self.find_result.find_rev.set(find_rev);
        }

        if self.find_result.progress.get_untracked() != FindProgress::Started {
            return;
        }

        let search = self.find.search_string.get_untracked();
        let search = match search {
            Some(search) => search,
            None => return,
        };
        if search.content.is_empty() {
            return;
        }

        self.find_result
            .progress
            .set(FindProgress::InProgress(Selection::new()));

        let find_result = self.find_result.clone();
        let send = create_ext_action(self.scope, move |occurrences: Selection| {
            find_result.occurrences.set(occurrences);
            find_result.progress.set(FindProgress::Ready);
        });

        let text = self.doc().buffer.with_untracked(|b| b.text().clone());
        let case_matching = self.find.case_matching.get_untracked();
        let whole_words = self.find.whole_words.get_untracked();
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

    /// Handle a pointer-down event. This is the critical focus management entry point.
    ///
    /// IMPORTANT: The `is_normal()` guard prevents preview editors (in search modal,
    /// global search panel, palette) from stealing app-level focus to `Focus::Workbench`.
    /// Without this guard, clicking a preview editor would close its parent popup.
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
            self.find_focus.set(false);
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
            self.cursor().update(|cursor| {
                cursor.set_offset(offset, true, pointer_event.modifiers.alt())
            });
        }
        self.update_diagnostic_hover(offset);

        // Cmd+hover definition link styling
        let is_cmd = (cfg!(target_os = "macos") && pointer_event.modifiers.meta())
            || (cfg!(not(target_os = "macos")) && pointer_event.modifiers.control());

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
                    && self.link_hover_range.get_untracked()
                        != Some((start_offset, end_offset))
                {
                    self.link_hover_range.set(None);
                    doc.clear_text_cache();

                    let link_hover_range = self.link_hover_range;
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
        } else if self.link_hover_range.get_untracked().is_some() {
            self.link_hover_range.set(None);
            self.doc().clear_text_cache();
        }
    }

    #[instrument]
    pub fn pointer_up(&self, pointer_event: &PointerInputEvent) {
        self.editor.pointer_up(pointer_event);
    }

    #[instrument]
    pub fn pointer_leave(&self) {
        self.mouse_hover_timer.set(TimerToken::INVALID);
        if self.link_hover_range.get_untracked().is_some() {
            self.link_hover_range.set(None);
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
    fn update_hover(&self, offset: usize) {
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

    // reset the doc inside and move cursor back
    pub fn reset(&self) {
        let doc = self.doc();
        doc.reload(Rope::from(""), true);
        self.cursor()
            .update(|cursor| cursor.set_offset(0, false, false));
    }

    pub fn visual_line(&self, line: usize) -> usize {
        line
    }

    pub fn actual_line(&self, visual_line: usize, _bottom_affinity: bool) -> usize {
        visual_line
    }
}

/// KeyPressFocus implementation routes keyboard events to the editor or its
/// sub-components (find/replace, completion list). The `check_condition` method
/// determines which keybinding conditions are active, controlling whether bindings
/// like list.next (up/down in completion) or editor-specific bindings fire.
impl KeyPressFocus for EditorData {
    #[instrument]
    fn check_condition(&self, condition: Condition) -> bool {
        match condition {
            Condition::InputFocus => {
                self.find.visual.get_untracked() && self.find_focus.get_untracked()
            }
            Condition::ListFocus => self.has_completions(),
            Condition::CompletionFocus => self.has_completions(),
            Condition::InlineCompletionVisible => self.has_inline_completions(),
            Condition::OnScreenFindActive => {
                self.on_screen_find.with_untracked(|f| f.active)
            }
            Condition::InSnippet => self.snippet.with_untracked(|s| s.is_some()),
            Condition::EditorFocus => self
                .doc()
                .content
                .with_untracked(|content| !content.is_local()),
            Condition::SearchFocus => {
                self.find.visual.get_untracked()
                    && self.find_focus.get_untracked()
                    && !self.find.replace_focus.get_untracked()
            }
            Condition::ReplaceFocus => {
                self.find.visual.get_untracked()
                    && self.find_focus.get_untracked()
                    && self.find.replace_focus.get_untracked()
            }
            Condition::SearchActive => self.find.visual.get_untracked(),
            _ => false,
        }
    }

    /// Command dispatch. When the find bar is focused, Edit and Move commands are
    /// forwarded to the find/replace editor via InternalCommand rather than being
    /// applied to the main editor. This allows typing in the find bar while the
    /// editor retains app-level focus (Focus::Workbench).
    #[instrument]
    fn run_command(
        &self,
        command: &crate::command::LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        if self.find.visual.get_untracked() && self.find_focus.get_untracked() {
            match &command.kind {
                CommandKind::Edit(_) | CommandKind::Move(_) => {
                    if self.find.replace_focus.get_untracked() {
                        if let Some(replace_ed) =
                            self.replace_editor_signal.get_untracked()
                        {
                            replace_ed.run_command(command, count, mods);
                        }
                    } else if let Some(find_ed) =
                        self.find_editor_signal.get_untracked()
                    {
                        find_ed.run_command(command, count, mods);
                    }
                    return CommandExecuted::Yes;
                }
                _ => {}
            }
        }

        match &command.kind {
            crate::command::CommandKind::Workbench(_) => CommandExecuted::No,
            crate::command::CommandKind::Edit(cmd) => self.run_edit_command(cmd),
            crate::command::CommandKind::Move(cmd) => {
                let movement = cmd.to_movement(count);
                self.run_move_command(&movement, count, mods)
            }
            crate::command::CommandKind::Scroll(cmd) => {
                if self
                    .doc()
                    .content
                    .with_untracked(|content| content.is_local())
                {
                    return CommandExecuted::No;
                }
                self.run_scroll_command(cmd, count, mods)
            }
            crate::command::CommandKind::Focus(cmd) => {
                if self
                    .doc()
                    .content
                    .with_untracked(|content| content.is_local())
                {
                    return CommandExecuted::No;
                }
                self.run_focus_command(cmd, count, mods)
            }
            crate::command::CommandKind::MotionMode(_) => CommandExecuted::No,
            crate::command::CommandKind::MultiSelection(_) => CommandExecuted::No,
        }
    }

    fn expect_char(&self) -> bool {
        if self.find.visual.get_untracked() && self.find_focus.get_untracked() {
            false
        } else {
            self.inline_find.with_untracked(|f| f.is_some())
                || self.on_screen_find.with_untracked(|f| f.active)
        }
    }

    /// Character input handler. Routes characters to the find/replace editors when
    /// they have focus, otherwise performs a normal insert into the editor buffer.
    /// Completion is triggered on non-whitespace input; inline completion updates
    /// on every character.
    fn receive_char(&self, c: &str) {
        if self.find.visual.get_untracked() && self.find_focus.get_untracked() {
            // find/replace editor receive char
            if self.find.replace_focus.get_untracked() {
                if let Some(replace_ed) = self.replace_editor_signal.get_untracked()
                {
                    replace_ed.receive_char(c);
                }
            } else if let Some(find_ed) = self.find_editor_signal.get_untracked() {
                find_ed.receive_char(c);
            }
        } else {
            // normal editor receive char
            let mut cursor = self.cursor().get_untracked();
            let deltas = self.doc().do_insert(
                &mut cursor,
                c,
                &self.common.config.get_untracked(),
            );
            self.cursor().set(cursor);

            if !c
                .chars()
                .all(|c| c.is_whitespace() || c.is_ascii_whitespace())
            {
                self.update_completion(false);
            } else {
                self.cancel_completion();
            }

            self.update_inline_completion(InlineCompletionTriggerKind::Automatic);

            self.apply_deltas(&deltas);
        }
    }
}

/// Custom signal wrapper for [`Doc`], because [`Editor`] only knows it as a
/// `Rc<dyn Document>`, and there is currently no way to have an `RwSignal<Rc<Doc>>` and
/// an `RwSignal<Rc<dyn Document>>`.
///
/// Every method here performs a downcast from `Rc<dyn Document>` to `Rc<Doc>`.
/// This is safe because Lapce always stores a `Doc` behind the trait object.
/// The `with` / `with_untracked` methods clone the `Rc` before downcasting because
/// `Rc::downcast` requires ownership; this is cheap (just a refcount bump).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocSignal {
    // TODO: replace with ReadSignal once that impls `track`
    inner: RwSignal<Rc<dyn Document>>,
}
impl DocSignal {
    pub fn get(&self) -> Rc<Doc> {
        let doc = self.inner.get();
        (doc as Rc<dyn ::std::any::Any>)
            .downcast()
            .expect("doc is not Rc<Doc>")
    }

    pub fn get_untracked(&self) -> Rc<Doc> {
        let doc = self.inner.get_untracked();
        (doc as Rc<dyn ::std::any::Any>)
            .downcast()
            .expect("doc is not Rc<Doc>")
    }

    pub fn with<O>(&self, f: impl FnOnce(&Rc<Doc>) -> O) -> O {
        self.inner.with(|doc| {
            let doc = doc.clone();
            let doc: Rc<Doc> = (doc as Rc<dyn ::std::any::Any>)
                .downcast()
                .expect("doc is not Rc<Doc>");
            f(&doc)
        })
    }

    pub fn with_untracked<O>(&self, f: impl FnOnce(&Rc<Doc>) -> O) -> O {
        self.inner.with_untracked(|doc| {
            let doc = doc.clone();
            let doc: Rc<Doc> = (doc as Rc<dyn ::std::any::Any>)
                .downcast()
                .expect("doc is not Rc<Doc>");
            f(&doc)
        })
    }

    pub fn track(&self) {
        self.inner.track();
    }
}

/// Determines whether autocompletion should be triggered after a delete command.
/// Only delete commands can trigger completion (not inserts -- those go through
/// `receive_char`). The deleted range is extracted from the delta to check if
/// only whitespace was removed; if so, completion is suppressed to avoid noisy
/// popups when cleaning up blank space.
fn show_completion(
    cmd: &EditCommand,
    doc: &Rope,
    deltas: &[(Rope, RopeDelta, InvalLines)],
) -> bool {
    match cmd {
        EditCommand::DeleteBackward
        | EditCommand::DeleteForward
        | EditCommand::DeleteWordBackward
        | EditCommand::DeleteWordForward
        | EditCommand::DeleteForwardAndInsert => {
            let start = match deltas.first().and_then(|delta| delta.1.els.first()) {
                Some(lapce_xi_rope::DeltaElement::Copy(_, start)) => *start,
                _ => 0,
            };

            let end = match deltas.first().and_then(|delta| delta.1.els.get(1)) {
                Some(lapce_xi_rope::DeltaElement::Copy(end, _)) => *end,
                _ => 0,
            };

            if start > 0 && end > start {
                !doc.slice_to_cow(start..end)
                    .chars()
                    .all(|c| c.is_whitespace() || c.is_ascii_whitespace())
            } else {
                true
            }
        }
        _ => false,
    }
}

fn show_inline_completion(cmd: &EditCommand) -> bool {
    matches!(
        cmd,
        EditCommand::DeleteBackward
            | EditCommand::DeleteForward
            | EditCommand::DeleteWordBackward
            | EditCommand::DeleteWordForward
            | EditCommand::DeleteForwardAndInsert
            | EditCommand::IndentLine
            | EditCommand::InsertMode
    )
}

/// Compute which visual lines are visible in the current viewport and build the
/// ScreenLines structure used by the paint pipeline. This maps viewport Y coordinates
/// to visual lines (VLine) and then to render lines (RVLine), which account for
/// line wrapping. The resulting `ScreenLines` is cached and used by all paint methods
/// to avoid redundant line-to-position calculations.
pub(crate) fn compute_screen_lines(
    config: ReadSignal<Arc<LapceConfig>>,
    base: RwSignal<ScreenLinesBase>,
    view_kind: ReadSignal<EditorViewKind>,
    doc: &Doc,
    lines: &Lines,
    text_prov: impl TextLayoutProvider + Clone,
    config_id: ConfigId,
) -> ScreenLines {
    // TODO: this should probably be a get since we need to depend on line-height
    let config = config.get();
    let line_height = config.editor.line_height();

    let (y0, y1) = base
        .with_untracked(|base| (base.active_viewport.y0, base.active_viewport.y1));
    // Get the start and end (visual) lines that are visible in the viewport
    let min_vline = VLine((y0 / line_height as f64).floor() as usize);
    let max_vline = VLine((y1 / line_height as f64).ceil() as usize);

    let cache_rev = doc.cache_rev.get();
    lines.check_cache_rev(cache_rev);
    // TODO(minor): we don't really need to depend on various subdetails that aren't affecting how
    // the screen lines are set up, like the title of a scratch document.
    doc.content.track();
    doc.loaded.track();

    let min_info = once_cell::sync::Lazy::new(|| {
        lines
            .iter_vlines(text_prov.clone(), false, min_vline)
            .next()
    });

    match view_kind.get() {
        EditorViewKind::Normal | EditorViewKind::Preview => {
            let mut rvlines = Vec::new();
            let mut info = HashMap::new();

            let Some(min_info) = *min_info else {
                return ScreenLines {
                    lines: Rc::new(rvlines),
                    info: Rc::new(info),
                    diff_sections: None,
                    base,
                };
            };

            // TODO: the original was min_line..max_line + 1, are we iterating too little now?
            // the iterator is from min_vline..max_vline
            let count = max_vline.get() - min_vline.get();
            let iter = lines.iter_rvlines_init(
                text_prov,
                cache_rev,
                config_id,
                min_info.rvline,
                false,
            );

            // let range = doc.folding_ranges.get().get_folded_range();
            // let mut init_index = 0;

            for (i, vline_info) in iter.enumerate() {
                if rvlines.len() >= count {
                    break;
                }

                // let (folded, next_index) =
                //     range.contain_line(init_index, vline_info.rvline.line as u32);
                // init_index = next_index;
                // if folded {
                //     continue;
                // }
                rvlines.push(vline_info.rvline);

                let y_idx = min_vline.get() + i;
                let vline_y = y_idx * line_height;
                let line_y = vline_y - vline_info.rvline.line_index * line_height;

                // Add the information to make it cheap to get in the future.
                // This y positions are shifted by the baseline y0
                info.insert(
                    vline_info.rvline,
                    LineInfo {
                        y: line_y as f64 - y0,
                        vline_y: vline_y as f64 - y0,
                        vline_info,
                    },
                );
            }

            ScreenLines {
                lines: Rc::new(rvlines),
                info: Rc::new(info),
                diff_sections: None,
                base,
            }
        }
    }
}

fn parse_hover_resp(
    hover: lsp_types::Hover,
    config: &LapceConfig,
) -> Vec<MarkdownContent> {
    match hover.contents {
        HoverContents::Scalar(text) => match text {
            MarkedString::String(text) => {
                parse_markdown(&text, LapceLayout::UI_LINE_HEIGHT, config)
            }
            MarkedString::LanguageString(code) => parse_markdown(
                &format!("```{}\n{}\n```", code.language, code.value),
                LapceLayout::UI_LINE_HEIGHT,
                config,
            ),
        },
        HoverContents::Array(array) => array
            .into_iter()
            .map(|t| from_marked_string(t, config))
            .rev()
            .reduce(|mut contents, more| {
                contents.push(MarkdownContent::Separator);
                contents.extend(more);
                contents
            })
            .unwrap_or_default(),
        HoverContents::Markup(content) => match content.kind {
            MarkupKind::PlainText => {
                from_plaintext(&content.value, LapceLayout::UI_LINE_HEIGHT, config)
            }
            MarkupKind::Markdown => {
                parse_markdown(&content.value, LapceLayout::UI_LINE_HEIGHT, config)
            }
        },
    }
}

#[derive(Debug)]
enum FindHintRs {
    NoMatchBreak,
    NoMatchContinue { pre_hint_len: u32 },
    MatchWithoutLocation,
    Match(Location),
}

fn find_hint(mut pre_hint_len: u32, index: u32, hint: &InlayHint) -> FindHintRs {
    use FindHintRs::*;
    match &hint.label {
        InlayHintLabel::String(text) => {
            let actual_col = pre_hint_len + hint.position.character;
            let actual_col_end = actual_col + (text.len() as u32);
            if actual_col > index {
                NoMatchBreak
            } else if actual_col <= index && index < actual_col_end {
                MatchWithoutLocation
            } else {
                pre_hint_len += text.len() as u32;
                NoMatchContinue { pre_hint_len }
            }
        }
        InlayHintLabel::LabelParts(parts) => {
            for part in parts {
                let actual_col = pre_hint_len + hint.position.character;
                let actual_col_end = actual_col + part.value.len() as u32;
                if index < actual_col {
                    return NoMatchBreak;
                } else if actual_col <= index && index < actual_col_end {
                    if let Some(location) = &part.location {
                        return Match(location.clone());
                    } else {
                        return MatchWithoutLocation;
                    }
                } else {
                    pre_hint_len += part.value.len() as u32;
                }
            }
            NoMatchContinue { pre_hint_len }
        }
    }
}

/// Removes Ruby stdlib locations when an equivalent gem version of the same file exists.
///
/// In modern Ruby (3.0+), many stdlib libraries were extracted into "default gems".
/// Both the legacy stdlib copy (e.g., `.../lib/ruby/3.4.0/uri/common.rb`) and the
/// gem copy (e.g., `.../gems/uri-1.1.1/lib/uri/common.rb`) coexist on disk, but only
/// the gem version is loaded at runtime. LSP servers like ruby-lsp find both, so we
/// Extend a word-start offset backward to include Ruby sigils (`@`, `@@`, `$`).
///
/// `prev_code_boundary` treats `@` as a non-word character, so for `@text` it
/// returns the offset of `t`. This function checks the preceding character(s) and
/// adjusts the offset to include the sigil, so the LSP receives the full symbol.
fn ruby_word_start(buffer: &Buffer, word_start: usize) -> usize {
    if word_start == 0 {
        return word_start;
    }
    let prev = buffer.slice_to_cow(word_start - 1..word_start);
    if prev == "@" {
        if word_start >= 2
            && buffer.slice_to_cow(word_start - 2..word_start - 1) == "@"
        {
            word_start - 2 // @@class_var
        } else {
            word_start - 1 // @instance_var
        }
    } else if prev == "$" {
        word_start - 1 // $global_var
    } else {
        word_start
    }
}

/// Returns true if the URI points to a Ruby type definition file (.rbs or .rbi).
fn is_ruby_type_file(uri: &lsp_types::Url) -> bool {
    let path = uri.path();
    path.ends_with(".rbs") || path.ends_with(".rbi")
}

/// Remove locations pointing to Ruby type definition files (.rbs, .rbi).
fn ruby_filter_type_files(locations: &mut Vec<Location>) {
    locations.retain(|l| !is_ruby_type_file(&l.uri));
}

/// drop the stdlib duplicate.
///
/// Detection: a stdlib path contains `/lib/ruby/<ver>/<relpath>` without `/gems/`.
/// A gem path contains `/gems/<name>/lib/<relpath>`. If `<relpath>` matches, the
/// stdlib entry is redundant.
fn dedup_ruby_stdlib_gems(locations: &mut Vec<Location>) {
    if locations.len() < 2 {
        return;
    }

    // Collect relative paths from gem locations: /gems/<gem-name-ver>/lib/<relpath>
    let gem_rel_paths: HashSet<String> = locations
        .iter()
        .filter_map(|l| {
            let path = l.uri.path();
            // Find the last /gems/<gem-name-ver>/lib/ and extract the relative path
            let mut search_from = 0;
            let mut result = None;
            while let Some(idx) = path[search_from..].find("/gems/") {
                let abs_idx = search_from + idx;
                let after = &path[abs_idx + "/gems/".len()..];
                if let Some(slash) = after.find('/') {
                    let rest = &after[slash + 1..];
                    if let Some(rel) = rest.strip_prefix("lib/") {
                        if !rel.is_empty() {
                            result = Some(rel.to_string());
                        }
                    }
                }
                search_from = abs_idx + 1;
            }
            result
        })
        .collect();

    if gem_rel_paths.is_empty() {
        return;
    }

    // Remove stdlib locations whose relative path matches a gem entry.
    // Stdlib pattern: /lib/ruby/<ver>/<relpath> where the segment is NOT "gems/"
    locations.retain(|l| {
        let path = l.uri.path();
        if let Some(idx) = path.find("/lib/ruby/") {
            let after = &path[idx + "/lib/ruby/".len()..];
            if !after.starts_with("gems/") {
                if let Some(slash) = after.find('/') {
                    let rel_path = &after[slash + 1..];
                    if !rel_path.is_empty() && gem_rel_paths.contains(rel_path) {
                        return false; // Drop this stdlib duplicate
                    }
                }
            }
        }
        true
    });
}
