use std::{collections::HashMap, rc::Rc, sync::Arc};

use floem::{
    action::TimerToken,
    keyboard::Modifiers,
    kurbo::{Point, Rect, Vec2},
    prelude::SignalTrack,
    reactive::{
        ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith, batch,
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
use lapce_core::{
    buffer::{
        InvalLines,
        rope_text::{RopeText, RopeTextVal},
    },
    command::{EditCommand, FocusCommand, ScrollCommand},
    cursor::Cursor,
    selection::SelRegion,
};
use lapce_rpc::buffer::BufferId;
use lapce_xi_rope::{Rope, RopeDelta};
use lsp_types::{
    HoverContents, InlayHint, InlayHintLabel, InlineCompletionTriggerKind, Location,
    MarkedString, MarkupKind,
};
use serde::{Deserialize, Serialize};
use view::StickyHeaderInfo;

use self::location::{EditorLocation, EditorPosition};
use crate::{
    command::{CommandKind, InternalCommand},
    config::{LapceConfig, layout::LapceLayout},
    doc::{Doc, DocContent},
    editor_tab::EditorTabChild,
    find::{Find, FindResult},
    id::EditorTabId,
    keypress::{KeyPressFocus, condition::Condition},
    main_split::{Editors, MainSplitData, SplitDirection, SplitMoveDirection},
    markdown::{
        MarkdownContent, from_marked_string, from_plaintext, parse_markdown,
    },
    tracing::*,
    workspace_data::{CommonData, WorkspaceData},
};

pub mod gutter;
pub mod location;
pub mod ops_completion;
pub mod ops_edit;
pub mod ops_file;
pub mod ops_lsp;
pub mod ops_navigation;
pub mod ops_pointer;
pub mod ops_search;
pub mod ops_selection;
pub mod ruby;
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

/// Per-editor state for every "find" affordance: the Ctrl+F find bar
/// (`find`/`find_result`/`find_focus` plus the floated find/replace input
/// editors), the vim-style inline find (`inline_find`/`last_inline_find`),
/// and the on-screen find highlighter (`on_screen_find`).
#[derive(Clone)]
pub struct EditorFindState {
    pub find: Find,
    pub find_result: FindResult,
    /// Whether the find/replace bar has keyboard focus (as opposed to the editor body).
    /// When true, typed characters are routed to the find/replace editors instead.
    pub find_focus: RwSignal<bool>,
    pub find_editor_signal: RwSignal<Option<EditorData>>,
    pub replace_editor_signal: RwSignal<Option<EditorData>>,
    pub inline_find: RwSignal<Option<InlineFindDirection>>,
    pub last_inline_find: RwSignal<Option<(InlineFindDirection, String)>>,
    pub on_screen_find: RwSignal<OnScreenFind>,
}

impl EditorFindState {
    pub fn new(cx: Scope) -> Self {
        Self {
            find: Find::new(cx),
            find_result: FindResult::new(cx),
            find_focus: cx.create_rw_signal(false),
            find_editor_signal: cx.create_rw_signal(None),
            replace_editor_signal: cx.create_rw_signal(None),
            inline_find: cx.create_rw_signal(None),
            last_inline_find: cx.create_rw_signal(None),
            on_screen_find: cx.create_rw_signal(OnScreenFind {
                active: false,
                pattern: "".to_string(),
                regions: Vec::new(),
            }),
        }
    }
}

/// Pointer-hover state for a single editor instance. Coordinates the debouncer
/// for diagnostic/definition hovers and tracks the confirmed Cmd-link range.
#[derive(Clone, Copy)]
pub struct HoverPointerState {
    /// Debounce timer used by the diagnostic hover popup.
    pub timer: RwSignal<TimerToken>,
    /// Cache of the last pointer-move inputs `(offset, is_inside, is_cmd)` that
    /// drove hover work. Used to short-circuit `pointer_move` when the mouse
    /// moves within the same character and modifier state -- the diagnostic
    /// hover scan and Cmd-link boundary lookup would produce identical results.
    pub state: RwSignal<Option<(usize, bool, bool)>>,
    /// Range `(start_offset, end_offset)` of the symbol with a confirmed
    /// definition link. Set when Cmd is held and the LSP confirms a definition
    /// exists.
    pub link_range: RwSignal<Option<(usize, usize)>>,
}

impl HoverPointerState {
    pub fn new(cx: Scope) -> Self {
        Self {
            timer: cx.create_rw_signal(TimerToken::INVALID),
            state: cx.create_rw_signal(None),
            link_range: cx.create_rw_signal(None),
        }
    }
}

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
    pub find_state: EditorFindState,
    pub editor: Rc<Editor>,
    /// Distinguishes normal (workbench) editors from preview editors. Preview editors
    /// skip sticky headers and don't steal focus on pointer_down.
    pub kind: RwSignal<EditorViewKind>,
    pub sticky_header_height: RwSignal<f64>,
    pub hover: HoverPointerState,
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
            find_state: EditorFindState::new(cx),
            editor: Rc::new(editor),
            kind: cx.create_rw_signal(EditorViewKind::Normal),
            sticky_header_height: cx.create_rw_signal(0.0),
            hover: HoverPointerState::new(cx),
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
        self.try_doc()
            .expect("EditorData doc must always be Rc<Doc>")
    }

    /// Try to get the document, returning `None` if the editor's doc signal
    /// has been disposed (e.g. a preview editor whose scope was cleaned up).
    pub fn try_doc(&self) -> Option<Rc<Doc>> {
        let doc = self.editor.doc_signal().try_get_untracked()?;
        (doc as Rc<dyn ::std::any::Any>).downcast::<Doc>().ok()
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

    /// Dispatcher for every `FocusCommand`. Large arm bodies live in focused
    /// `cmd_*` helpers below; trivial arms stay inline. Tracks the completion
    /// index around dispatch so a list-movement command also refreshes the
    /// completion-lens preview doc.
    pub fn run_focus_command(
        &self,
        cmd: &FocusCommand,
        _count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        let prev_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        match cmd {
            // Modal / list navigation
            FocusCommand::ModalClose => self.cancel_completion(),
            FocusCommand::ListNext => self.common.completion.update(|c| c.next()),
            FocusCommand::ListPrevious => {
                self.common.completion.update(|c| c.previous())
            }
            FocusCommand::ListNextPage => {
                self.common.completion.update(|c| c.next_page())
            }
            FocusCommand::ListPreviousPage => {
                self.common.completion.update(|c| c.previous_page())
            }
            FocusCommand::ListSelect => {
                self.select_completion();
                self.cancel_inline_completion();
            }

            // Split management — all seven dispatch through one helper
            FocusCommand::SplitVertical
            | FocusCommand::SplitHorizontal
            | FocusCommand::SplitRight
            | FocusCommand::SplitLeft
            | FocusCommand::SplitUp
            | FocusCommand::SplitDown
            | FocusCommand::SplitExchange => {
                if !self.cmd_split_action(cmd) {
                    return CommandExecuted::No;
                }
            }
            FocusCommand::SplitClose => {
                if !self.cmd_split_close() {
                    return CommandExecuted::No;
                }
            }

            // Snippet placeholder navigation
            FocusCommand::JumpToNextSnippetPlaceholder => {
                self.cmd_jump_snippet_placeholder(true)
            }
            FocusCommand::JumpToPrevSnippetPlaceholder => {
                self.cmd_jump_snippet_placeholder(false)
            }

            // LSP actions
            FocusCommand::GotoDefinition => self.go_to_definition(),
            FocusCommand::ShowCodeActions => self.show_code_actions(false),
            FocusCommand::Rename => self.rename(),
            FocusCommand::ShowHover => self.cmd_show_hover(),

            // Search / replace
            FocusCommand::SearchWholeWordForward => {
                self.search_whole_word_forward(mods)
            }
            FocusCommand::SearchForward => self.search_forward(mods),
            FocusCommand::SearchBackward => self.search_backward(mods),
            FocusCommand::ClearSearch => self.clear_search(),
            FocusCommand::Search => self.search(),
            FocusCommand::SearchAndReplace => self.search_and_replace(),
            FocusCommand::ReplaceNext => self.replace_next_and_advance(),
            FocusCommand::ReplaceAll => self.replace_all_from_command(),
            FocusCommand::FocusFindEditor => {
                self.find_state.find.replace_focus.set(false)
            }
            FocusCommand::FocusReplaceEditor => {
                if self.find_state.find.replace_active.get_untracked() {
                    self.find_state.find.replace_focus.set(true);
                }
            }

            // File
            FocusCommand::Save => self.save(true, || {}),
            FocusCommand::SaveWithoutFormatting => self.save(false, || {}),
            FocusCommand::FormatDocument => self.format(),

            // Inline / on-screen find (type-ahead)
            FocusCommand::InlineFindLeft => self
                .find_state
                .inline_find
                .set(Some(InlineFindDirection::Left)),
            FocusCommand::InlineFindRight => self
                .find_state
                .inline_find
                .set(Some(InlineFindDirection::Right)),
            FocusCommand::OnScreenFind => {
                self.find_state.on_screen_find.update(|find| {
                    find.active = true;
                    find.pattern.clear();
                    find.regions.clear();
                });
            }
            FocusCommand::RepeatLastInlineFind => {
                if let Some((direction, c)) =
                    self.find_state.last_inline_find.get_untracked()
                {
                    self.inline_find(direction, &c);
                }
            }

            // Inline completion
            FocusCommand::InlineCompletionSelect => self.select_inline_completion(),
            FocusCommand::InlineCompletionNext => self.next_inline_completion(),
            FocusCommand::InlineCompletionPrevious => {
                self.previous_inline_completion()
            }
            FocusCommand::InlineCompletionCancel => self.cancel_inline_completion(),
            FocusCommand::InlineCompletionInvoke => {
                self.update_inline_completion(InlineCompletionTriggerKind::Invoked)
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

    /// Dispatch any of the seven split commands. Returns `false` when the
    /// editor is a local/preview one (no `editor_tab_id`), so `run_focus_command`
    /// can propagate `CommandExecuted::No`.
    fn cmd_split_action(&self, cmd: &FocusCommand) -> bool {
        let Some(editor_tab_id) = self.editor_tab_id.get_untracked() else {
            return false;
        };
        let action = match cmd {
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
            _ => unreachable!("cmd_split_action called with non-split command"),
        };
        self.common.internal_command.send(action);
        true
    }

    fn cmd_split_close(&self) -> bool {
        let Some(editor_tab_id) = self.editor_tab_id.get_untracked() else {
            return false;
        };
        self.common
            .internal_command
            .send(InternalCommand::EditorTabChildClose {
                editor_tab_id,
                child: EditorTabChild::Editor(self.id()),
            });
        true
    }

    /// Move the cursor to the next/previous snippet placeholder. Clears the
    /// snippet when advancing past the last placeholder.
    fn cmd_jump_snippet_placeholder(&self, forward: bool) {
        self.snippet.update(|snippet| {
            let Some(snippet_mut) = snippet.as_mut() else {
                return;
            };
            let mut current = 0;
            let offset = self.cursor().get_untracked().offset();
            for (i, (_, (start, end))) in snippet_mut.iter().enumerate() {
                if *start <= offset && offset <= *end {
                    current = i;
                    break;
                }
            }

            let target = if forward {
                Some(current + 1)
            } else if current > 0 {
                Some(current - 1)
            } else {
                None
            };
            let Some(target) = target else {
                return;
            };

            let last_placeholder =
                forward && target >= snippet_mut.len().saturating_sub(1);

            if let Some((_, (start, end))) = snippet_mut.get(target) {
                let mut selection = lapce_core::selection::Selection::new();
                selection.add_region(lapce_core::selection::SelRegion::new(
                    *start, *end, None,
                ));
                self.cursor().update(|cursor| cursor.set_insert(selection));
            }

            if last_placeholder {
                *snippet = None;
            }
            self.cancel_completion();
            self.cancel_inline_completion();
        });
    }

    fn cmd_show_hover(&self) {
        let start_offset = self.doc().buffer.with_untracked(|b| {
            b.prev_code_boundary(self.cursor().get_untracked().offset())
        });
        self.update_hover(start_offset);
    }

    fn scroll(&self, down: bool, count: usize, mods: Modifiers) {
        self.editor.scroll(
            self.sticky_header_height.get_untracked(),
            down,
            count,
            mods,
        )
    }

    /// Handle a pointer-down event. This is the critical focus management entry point.
    ///
    /// IMPORTANT: The `is_normal()` guard prevents preview editors (in search modal,
    /// global search panel, palette) from stealing app-level focus to `Focus::Workbench`.
    /// Without this guard, clicking a preview editor would close its parent popup.

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
                self.find_state.find.visual.get_untracked()
                    && self.find_state.find_focus.get_untracked()
            }
            Condition::ListFocus => self.has_completions(),
            Condition::CompletionFocus => self.has_completions(),
            Condition::InlineCompletionVisible => self.has_inline_completions(),
            Condition::OnScreenFindActive => {
                self.find_state.on_screen_find.with_untracked(|f| f.active)
            }
            Condition::InSnippet => self.snippet.with_untracked(|s| s.is_some()),
            Condition::EditorFocus => self
                .doc()
                .content
                .with_untracked(|content| !content.is_local()),
            Condition::SearchFocus => {
                self.find_state.find.visual.get_untracked()
                    && self.find_state.find_focus.get_untracked()
                    && !self.find_state.find.replace_focus.get_untracked()
            }
            Condition::ReplaceFocus => {
                self.find_state.find.visual.get_untracked()
                    && self.find_state.find_focus.get_untracked()
                    && self.find_state.find.replace_focus.get_untracked()
            }
            Condition::SearchActive => self.find_state.find.visual.get_untracked(),
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
        if self.find_state.find.visual.get_untracked()
            && self.find_state.find_focus.get_untracked()
        {
            match &command.kind {
                CommandKind::Edit(_) | CommandKind::Move(_) => {
                    if self.find_state.find.replace_focus.get_untracked() {
                        if let Some(replace_ed) =
                            self.find_state.replace_editor_signal.get_untracked()
                        {
                            replace_ed.run_command(command, count, mods);
                        }
                    } else if let Some(find_ed) =
                        self.find_state.find_editor_signal.get_untracked()
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
        if self.find_state.find.visual.get_untracked()
            && self.find_state.find_focus.get_untracked()
        {
            false
        } else {
            self.find_state.inline_find.with_untracked(|f| f.is_some())
                || self.find_state.on_screen_find.with_untracked(|f| f.active)
        }
    }

    /// Character input handler. Routes characters to the find/replace editors when
    /// they have focus, otherwise performs a normal insert into the editor buffer.
    /// Completion is triggered on non-whitespace input; inline completion updates
    /// on every character.
    fn receive_char(&self, c: &str) {
        if self.find_state.find.visual.get_untracked()
            && self.find_state.find_focus.get_untracked()
        {
            // find/replace editor receive char
            if self.find_state.find.replace_focus.get_untracked() {
                if let Some(replace_ed) =
                    self.find_state.replace_editor_signal.get_untracked()
                {
                    replace_ed.receive_char(c);
                }
            } else if let Some(find_ed) =
                self.find_state.find_editor_signal.get_untracked()
            {
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

pub(crate) fn parse_hover_resp(
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
pub(crate) enum FindHintRs {
    NoMatchBreak,
    NoMatchContinue { pre_hint_len: u32 },
    MatchWithoutLocation,
    Match(Location),
}

pub(crate) fn find_hint(
    mut pre_hint_len: u32,
    index: u32,
    hint: &InlayHint,
) -> FindHintRs {
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
