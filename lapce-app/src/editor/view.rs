use std::{cmp, ops::DerefMut, rc::Rc, sync::Arc};

use floem::{
    Renderer, View, ViewId,
    action::{set_ime_allowed, set_ime_cursor_area},
    context::{PaintCx, StyleCx},
    event::{Event, EventListener, EventPropagation},
    keyboard::Modifiers,
    kurbo::Stroke,
    peniko::{
        Color,
        kurbo::{Line, Point, Rect, Size},
    },
    prelude::SignalTrack,
    reactive::{
        Memo, ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith,
        create_effect, create_memo, create_rw_signal,
    },
    style::{CursorColor, CursorStyle, Style, TextColor},
    taffy::prelude::NodeId,
    views::{
        Decorators, clip, container, dyn_stack,
        editor::{
            CurrentLineColor, CursorSurroundingLines, Editor, EditorStyle,
            IndentGuideColor, IndentStyleProp, Modal, PhantomColor,
            PlaceholderColor, PreeditUnderlineColor, RenderWhitespaceProp,
            ScrollBeyondLastLine, SelectionColor, ShowIndentGuide, SmartTab,
            VisibleWhitespaceColor, WrapProp,
            text::WrapMethod,
            view::{
                EditorView as FloemEditorView, EditorViewClass, LineRegion,
                ScreenLines, cursor_caret,
            },
            visual_line::RVLine,
        },
        empty, label,
        scroll::{PropagatePointerWheel, scroll},
        stack, svg,
    },
};
use lapce_core::{
    buffer::{Buffer, rope_text::RopeText},
    cursor::{CursorAffinity, CursorMode},
    selection::SelRegion,
};
use lapce_rpc::plugin::PluginId;
use lapce_xi_rope::find::CaseMatching;
use lsp_types::CodeLens;

use super::{DocSignal, EditorData, EditorViewKind, gutter::editor_gutter_view};
use crate::config::layout::LapceLayout;
use crate::{
    app::clickable_icon,
    command::InternalCommand,
    config::{LapceConfig, color::LapceColor, editor::WrapStyle, icon::LapceIcons},
    doc::DocContent,
    editor::gutter::FoldingDisplayItem,
    text_input::TextInputBuilder,
    workspace::LapceWorkspace,
    workspace_data::{Focus, WorkspaceData},
};

#[derive(Clone, Debug, Default)]
pub struct StickyHeaderInfo {
    pub sticky_lines: Vec<usize>,
    pub last_sticky_should_scroll: bool,
    pub y_diff: f64,
}

fn editor_wrap(config: &LapceConfig) -> WrapMethod {
    /// Minimum width that we'll allow the view to be wrapped at.
    const MIN_WRAPPED_WIDTH: f32 = 100.0;

    match config.editor.wrap_style {
        WrapStyle::None => WrapMethod::None,
        WrapStyle::EditorWidth => WrapMethod::EditorWidth,
        WrapStyle::WrapWidth => WrapMethod::WrapWidth {
            width: (config.editor.wrap_width as f32).max(MIN_WRAPPED_WIDTH),
        },
    }
}

pub fn editor_style(
    config: ReadSignal<Arc<LapceConfig>>,
    doc: DocSignal,
    s: Style,
) -> Style {
    let config = config.get();
    let doc = doc.get();

    s.set(
        IndentStyleProp,
        doc.buffer.with_untracked(Buffer::indent_style),
    )
    .set(CursorColor, config.color(LapceColor::EDITOR_CARET))
    .set(SelectionColor, config.color(LapceColor::EDITOR_SELECTION))
    .set(
        CurrentLineColor,
        config.color(LapceColor::EDITOR_CURRENT_LINE),
    )
    .set(
        VisibleWhitespaceColor,
        config.color(LapceColor::EDITOR_VISIBLE_WHITESPACE),
    )
    .set(
        IndentGuideColor,
        config.color(LapceColor::EDITOR_INDENT_GUIDE),
    )
    .set(ScrollBeyondLastLine, config.editor.scroll_beyond_last_line)
    .color(config.color(LapceColor::EDITOR_FOREGROUND))
    .set(TextColor, config.color(LapceColor::EDITOR_FOREGROUND))
    .set(PhantomColor, config.color(LapceColor::EDITOR_DIM))
    .set(PlaceholderColor, config.color(LapceColor::EDITOR_DIM))
    .set(
        PreeditUnderlineColor,
        config.color(LapceColor::EDITOR_FOREGROUND),
    )
    .set(ShowIndentGuide, config.editor.show_indent_guide)
    .set(Modal, false)
    .set(SmartTab, config.editor.smart_tab)
    .set(WrapProp, editor_wrap(&config))
    .set(
        CursorSurroundingLines,
        config.editor.cursor_surrounding_lines,
    )
    .set(RenderWhitespaceProp, config.editor.render_whitespace)
}

pub struct EditorView {
    id: ViewId,
    editor: EditorData,
    is_active: Memo<bool>,
    inner_node: Option<NodeId>,
    viewport: RwSignal<Rect>,
}

/// Create the core editor view widget. This sets up reactive effects that trigger
/// relayout/repaint when the document, view kind, cursor, or buffer revision changes.
/// The sticky header computation is also wired as an effect here, using a revision
/// tuple to avoid redundant recalculations.
///
/// IME handling is set up via event listeners: `ImePreedit` for composition preview
/// text and `ImeCommit` for finalized input. The `is_active` guard prevents inactive
/// editors (in non-focused splits) from consuming IME events.
pub fn editor_view(
    e_data: EditorData,
    is_active: impl Fn(bool) -> bool + 'static + Copy,
) -> EditorView {
    let id = ViewId::new();
    let is_active = create_memo(move |_| is_active(true));

    let viewport = e_data.viewport();

    let doc = e_data.doc_signal();
    let view_kind = e_data.kind;
    let screen_lines = e_data.screen_lines();
    create_effect(move |_| {
        doc.track();
        view_kind.track();
        id.request_layout();
    });

    let hide_cursor = e_data.common.window_common.hide_cursor;
    let find_result_occurrences = e_data.find_result.occurrences;
    create_effect(move |_| {
        hide_cursor.track();
        find_result_occurrences.track();
        id.request_paint();
    });

    create_effect(move |last_rev| {
        let buffer = doc.with(|doc| doc.buffer);
        let rev = buffer.with(|buffer| buffer.rev());
        if last_rev == Some(rev) {
            return rev;
        }
        id.request_layout();
        rev
    });

    let config = e_data.common.config;
    let sticky_header_height_signal = e_data.sticky_header_height;
    let editor2 = e_data.clone();
    create_effect(move |last_rev| {
        let config = config.get();
        if !config.editor.sticky_header {
            return (DocContent::Local, 0, 0, Rect::ZERO, 0, None);
        }

        let doc = doc.get();
        let rect = viewport.get();
        let (screen_lines_len, screen_lines_first) = screen_lines
            .with(|lines| (lines.lines.len(), lines.lines.first().copied()));
        let rev = (
            doc.content.get(),
            doc.buffer.with(|b| b.rev()),
            doc.cache_rev.get(),
            rect,
            screen_lines_len,
            screen_lines_first,
        );
        if last_rev.as_ref() == Some(&rev) {
            return rev;
        }

        let sticky_header_info = get_sticky_header_info(
            &editor2,
            viewport,
            sticky_header_height_signal,
            &config,
        );

        id.update_state(sticky_header_info);

        rev
    });

    let ed1 = e_data.editor.clone();
    let ed2 = ed1.clone();
    let ed3 = ed1.clone();

    let editor_window_origin = e_data.window_origin();
    let cursor = e_data.cursor();
    let find_focus = e_data.find_focus;
    let ime_allowed = e_data.common.window_common.ime_allowed;
    let editor_viewport = e_data.viewport();
    let editor_cursor = e_data.cursor();
    create_effect(move |_| {
        let active = is_active.get();
        if active && !find_focus.get() {
            if !cursor.with(|c| c.is_insert()) {
                if ime_allowed.get_untracked() {
                    ime_allowed.set(false);
                    set_ime_allowed(false);
                }
            } else {
                if !ime_allowed.get_untracked() {
                    ime_allowed.set(true);
                    set_ime_allowed(true);
                }
                let (offset, affinity) = cursor.with(|c| (c.offset(), c.affinity));
                let (_, point_below) = ed1.points_of_offset(offset, affinity);
                let window_origin = editor_window_origin.get();
                let viewport = editor_viewport.get();
                let pos = window_origin
                    + (point_below.x - viewport.x0, point_below.y - viewport.y0);
                set_ime_cursor_area(
                    pos,
                    Size::new(
                        LapceLayout::DEFAULT_WINDOW_WIDTH,
                        LapceLayout::DEFAULT_WINDOW_HEIGHT,
                    ),
                );
            }
        }
    });

    let doc = e_data.doc_signal();
    EditorView {
        id,
        editor: e_data,
        is_active,
        inner_node: None,
        viewport,
    }
    .on_event(EventListener::ImePreedit, move |event| {
        if !is_active.get_untracked() {
            return EventPropagation::Continue;
        }

        if let Event::ImePreedit { text, cursor } = event {
            if text.is_empty() {
                ed2.clear_preedit();
            } else {
                let offset = editor_cursor.with_untracked(|c| c.offset());
                ed2.set_preedit(text.clone(), *cursor, offset);
            }
        }
        EventPropagation::Stop
    })
    .on_event(EventListener::ImeCommit, move |event| {
        if !is_active.get_untracked() {
            return EventPropagation::Continue;
        }

        if let Event::ImeCommit(text) = event {
            ed3.clear_preedit();
            ed3.receive_char(text);
        }
        EventPropagation::Stop
    })
    .class(EditorViewClass)
    .style(move |s| editor_style(config, doc, s))
}

impl EditorView {
    fn paint_current_line(
        &self,
        cx: &mut PaintCx,
        is_local: bool,
        screen_lines: &ScreenLines,
    ) {
        let e_data = self.editor.clone();
        let ed = e_data.editor.clone();
        let cursor = self.editor.cursor();
        let config = self.editor.common.config;

        let config = config.get_untracked();
        let line_height = config.editor.line_height() as f64;
        let viewport = self.viewport.get_untracked();

        let current_line_color = ed.es.with_untracked(EditorStyle::current_line);

        cursor.with_untracked(|cursor| {
            let highlight_current_line = match cursor.mode {
                CursorMode::Normal(_) | CursorMode::Insert(_) => true,
                CursorMode::Visual { .. } => false,
            };

            if let Some(current_line_color) = current_line_color {
                // Highlight the current line
                if !is_local && highlight_current_line {
                    for (_, end) in cursor.regions_iter() {
                        // TODO: unsure if this is correct for wrapping lines
                        let rvline = ed.rvline_of_offset(end, cursor.affinity);

                        if let Some(info) = screen_lines.info(rvline) {
                            let rect = Rect::from_origin_size(
                                (viewport.x0, info.vline_y),
                                (viewport.width(), line_height),
                            );

                            cx.fill(&rect, current_line_color, 0.0);
                        }
                    }
                }
            }
        });
    }

    fn paint_find(&self, cx: &mut PaintCx, screen_lines: &ScreenLines) {
        if !self.editor.kind.get_untracked().is_normal() {
            return;
        }
        let find_visual = self.editor.find.visual.get_untracked();
        if !find_visual && self.editor.on_screen_find.with_untracked(|f| !f.active) {
            return;
        }
        if screen_lines.lines.is_empty() {
            return;
        }

        let min_vline = *screen_lines.lines.first().unwrap();
        let max_vline = *screen_lines.lines.last().unwrap();
        let min_line = screen_lines.info(min_vline).unwrap().vline_info.rvline.line;
        let max_line = screen_lines.info(max_vline).unwrap().vline_info.rvline.line;

        let e_data = &self.editor;
        let ed = &e_data.editor;

        let config = self.editor.common.config;
        let occurrences = e_data.find_result.occurrences;

        let config = config.get_untracked();
        let line_height = config.editor.line_height() as f64;
        let match_bg = config.color(LapceColor::EDITOR_FIND_MATCH_BACKGROUND);
        let current_match_bg =
            config.color(LapceColor::EDITOR_FIND_CURRENT_MATCH_BACKGROUND);
        let current_match_border =
            config.color(LapceColor::EDITOR_FIND_CURRENT_MATCH_BORDER);

        let cursor_offset = self.editor.cursor().with_untracked(|c| c.offset());

        let start = ed.offset_of_line(min_line);
        let end = ed.offset_of_line(max_line + 1);

        if find_visual {
            self.editor.update_find();
            for region in occurrences.with_untracked(|selection| {
                selection.regions_in_range(start, end).to_vec()
            }) {
                let is_current =
                    cursor_offset >= region.min() && cursor_offset <= region.max();
                self.paint_find_region(
                    cx,
                    ed,
                    &region,
                    is_current,
                    match_bg,
                    current_match_bg,
                    current_match_border,
                    screen_lines,
                    line_height,
                );
            }
        }

        self.editor.on_screen_find.with_untracked(|find| {
            if find.active {
                for region in &find.regions {
                    let is_current = cursor_offset >= region.min()
                        && cursor_offset <= region.max();
                    self.paint_find_region(
                        cx,
                        ed,
                        region,
                        is_current,
                        match_bg,
                        current_match_bg,
                        current_match_border,
                        screen_lines,
                        line_height,
                    );
                }
            }
        });
    }

    fn paint_find_region(
        &self,
        cx: &mut PaintCx,
        ed: &Editor,
        region: &SelRegion,
        is_current: bool,
        match_bg: Color,
        current_match_bg: Color,
        current_match_border: Color,
        screen_lines: &ScreenLines,
        line_height: f64,
    ) {
        let start = region.min();
        let end = region.max();

        // TODO(minor): the proper affinity here should probably be tracked by selregion
        let (start_rvline, start_col) =
            ed.rvline_col_of_offset(start, CursorAffinity::Forward);
        let (end_rvline, end_col) =
            ed.rvline_col_of_offset(end, CursorAffinity::Backward);

        for line_info in screen_lines.iter_line_info() {
            let rvline_info = line_info.vline_info;
            let rvline = rvline_info.rvline;
            let line = rvline.line;

            if rvline < start_rvline {
                continue;
            }

            if rvline > end_rvline {
                break;
            }

            let left_col = if rvline == start_rvline { start_col } else { 0 };
            let (right_col, _vline_end) = if rvline == end_rvline {
                let max_col = ed.last_col(rvline_info, true);
                (end_col.min(max_col), false)
            } else {
                (ed.last_col(rvline_info, true), true)
            };

            // TODO(minor): sel region should have the affinity of the start/end
            let x0 = ed
                .line_point_of_line_col(
                    line,
                    left_col,
                    CursorAffinity::Forward,
                    true,
                )
                .x;
            let x1 = ed
                .line_point_of_line_col(
                    line,
                    right_col,
                    CursorAffinity::Backward,
                    true,
                )
                .x;

            if !rvline_info.is_empty() && start != end && left_col != right_col {
                let rect = Size::new(x1 - x0, line_height)
                    .to_rect()
                    .with_origin(Point::new(x0, line_info.vline_y));
                if is_current {
                    cx.fill(&rect, current_match_bg, 0.0);
                    cx.stroke(&rect, current_match_border, &Stroke::new(1.0));
                } else {
                    cx.fill(&rect, match_bg, 0.0);
                }
            }
        }
    }

    fn paint_sticky_headers(
        &self,
        cx: &mut PaintCx,
        viewport: Rect,
        screen_lines: &ScreenLines,
    ) {
        let config = self.editor.common.config.get_untracked();
        if !config.editor.sticky_header {
            return;
        }
        if !self.editor.kind.get_untracked().is_normal() {
            return;
        }

        let line_height = config.editor.line_height();
        let Some(start_vline) = screen_lines.lines.first() else {
            return;
        };
        let start_info = screen_lines.vline_info(*start_vline).unwrap();
        let start_line = start_info.rvline.line;

        let sticky_header_info = self.editor.sticky_header_info.get_untracked();
        let total_sticky_lines = sticky_header_info.sticky_lines.len();

        let paint_last_line = total_sticky_lines > 0
            && (sticky_header_info.last_sticky_should_scroll
                || sticky_header_info.y_diff != 0.0
                || start_line + total_sticky_lines - 1
                    != *sticky_header_info.sticky_lines.last().unwrap());

        let total_sticky_lines = if paint_last_line {
            total_sticky_lines
        } else {
            total_sticky_lines.saturating_sub(1)
        };

        if total_sticky_lines == 0 {
            return;
        }

        let scroll_offset = if sticky_header_info.last_sticky_should_scroll {
            sticky_header_info.y_diff
        } else {
            0.0
        };

        // Clear background

        let area_height = sticky_header_info
            .sticky_lines
            .iter()
            .copied()
            .map(|line| {
                let layout = self.editor.editor.text_layout(line);
                layout.line_count() * line_height
            })
            .sum::<usize>() as f64
            - scroll_offset;

        let sticky_area_rect = Size::new(viewport.x1, area_height)
            .to_rect()
            .with_origin(Point::new(0.0, viewport.y0))
            .inflate(10.0, 0.0);

        cx.fill(
            &sticky_area_rect,
            config.color(LapceColor::LAPCE_DROPDOWN_SHADOW),
            3.0,
        );
        cx.fill(
            &sticky_area_rect,
            config.color(LapceColor::EDITOR_STICKY_HEADER_BACKGROUND),
            0.0,
        );
        self.editor.sticky_header_info.get_untracked();
        // Paint lines
        let mut y_accum = 0.0;
        for (i, line) in sticky_header_info.sticky_lines.iter().copied().enumerate()
        {
            let y_diff = if i == total_sticky_lines - 1 {
                scroll_offset
            } else {
                0.0
            };

            let text_layout = self.editor.editor.text_layout(line);

            let text_height = (text_layout.line_count() * line_height) as f64;
            let height = text_height - y_diff;

            cx.save();

            let line_area_rect = Size::new(viewport.width(), height)
                .to_rect()
                .with_origin(Point::new(viewport.x0, viewport.y0 + y_accum));

            cx.clip(&line_area_rect);

            let y = viewport.y0 - y_diff + y_accum;
            cx.draw_text(&text_layout.text, Point::new(viewport.x0, y));

            y_accum += text_height;

            cx.restore();
        }
    }

    fn paint_scroll_bar(
        &self,
        cx: &mut PaintCx,
        viewport: Rect,
        is_local: bool,
        config: Arc<LapceConfig>,
    ) {
        const BAR_WIDTH: f64 = 10.0;

        if is_local {
            return;
        }

        cx.fill(
            &Rect::ZERO
                .with_size(Size::new(1.0, viewport.height()))
                .with_origin(Point::new(
                    viewport.x0 + viewport.width() - BAR_WIDTH,
                    viewport.y0,
                ))
                .inflate(0.0, 10.0),
            config.color(LapceColor::LAPCE_SCROLL_BAR),
            0.0,
        );
    }

    /// Paint a highlight around the characters at the given positions.
    fn paint_char_highlights(
        &self,
        cx: &mut PaintCx,
        screen_lines: &ScreenLines,
        highlight_line_cols: impl Iterator<Item = (RVLine, usize)>,
    ) {
        let editor = &self.editor.editor;
        let config = self.editor.common.config.get_untracked();
        let line_height = config.editor.line_height() as f64;

        for (rvline, col) in highlight_line_cols {
            // Is the given line on screen?
            if let Some(line_info) = screen_lines.info(rvline) {
                let x0 = editor
                    .line_point_of_line_col(
                        rvline.line,
                        col,
                        CursorAffinity::Forward,
                        true,
                    )
                    .x;
                let x1 = editor
                    .line_point_of_line_col(
                        rvline.line,
                        col + 1,
                        CursorAffinity::Backward,
                        true,
                    )
                    .x;

                let y0 = line_info.vline_y;
                let y1 = y0 + line_height;

                let rect = Rect::new(x0, y0, x1, y1);

                cx.stroke(
                    &rect,
                    config.color(LapceColor::EDITOR_FOREGROUND),
                    &Stroke::new(1.0),
                );
            }
        }
    }

    /// Paint scope lines between matching brackets. Draws an L-shaped indicator:
    /// a vertical line along the left edge of the scope, with horizontal lines
    /// connecting to the opening and closing brackets. The vertical line is
    /// positioned at the minimum indentation of the enclosed lines, creating a
    /// visual guide that follows the code structure. When brackets are on the
    /// same line, only a horizontal underline is drawn.
    fn paint_scope_lines(
        &self,
        cx: &mut PaintCx,
        viewport: Rect,
        screen_lines: &ScreenLines,
        (start, start_col): (RVLine, usize),
        (end, end_col): (RVLine, usize),
    ) {
        let editor = &self.editor.editor;
        let doc = self.editor.doc();
        let config = self.editor.common.config.get_untracked();
        let line_height = config.editor.line_height() as f64;
        let brush = config.color(LapceColor::EDITOR_FOREGROUND);

        if start == end {
            if let Some(line_info) = screen_lines.info(start) {
                // TODO: Due to line wrapping the y positions of these two spots could be different, do we need to change it?
                let x0 = editor
                    .line_point_of_line_col(
                        start.line,
                        start_col + 1,
                        CursorAffinity::Forward,
                        true,
                    )
                    .x;
                let x1 = editor
                    .line_point_of_line_col(
                        end.line,
                        end_col,
                        CursorAffinity::Backward,
                        true,
                    )
                    .x;

                if x0 < x1 {
                    let y = line_info.vline_y + line_height;

                    let p0 = Point::new(x0, y);
                    let p1 = Point::new(x1, y);
                    let line = Line::new(p0, p1);

                    cx.stroke(&line, brush, &Stroke::new(1.0));
                }
            }
        } else {
            // Are start_line and end_line on screen?
            let start_line_y = screen_lines
                .info(start)
                .map(|line_info| line_info.vline_y + line_height);
            let end_line_y = screen_lines
                .info(end)
                .map(|line_info| line_info.vline_y + line_height);

            // We only need to draw anything if start_line is on or before the visible section and
            // end_line is on or after the visible section.
            let y0 = start_line_y.or_else(|| {
                screen_lines
                    .lines
                    .first()
                    .is_some_and(|&first_vline| first_vline > start)
                    .then(|| viewport.min_y())
            });
            let y1 = end_line_y.or_else(|| {
                screen_lines
                    .lines
                    .last()
                    .is_some_and(|&last_line| last_line < end)
                    .then(|| viewport.max_y())
            });

            if let [Some(y0), Some(y1)] = [y0, y1] {
                let start_x = editor
                    .line_point_of_line_col(
                        start.line,
                        start_col + 1,
                        CursorAffinity::Forward,
                        true,
                    )
                    .x;
                let end_x = editor
                    .line_point_of_line_col(
                        end.line,
                        end_col,
                        CursorAffinity::Backward,
                        true,
                    )
                    .x;

                // TODO(minor): is this correct with line wrapping?
                // The vertical line should be drawn to the left of any non-whitespace characters
                // in the enclosed section.
                let min_text_x = doc.buffer.with_untracked(|buffer| {
                    ((start.line + 1)..=end.line)
                        .filter(|&line| !buffer.is_line_whitespace(line))
                        .map(|line| {
                            let non_blank_offset =
                                buffer.first_non_blank_character_on_line(line);
                            let (_, col) =
                                editor.offset_to_line_col(non_blank_offset);

                            editor
                                .line_point_of_line_col(
                                    line,
                                    col,
                                    CursorAffinity::Backward,
                                    true,
                                )
                                .x
                        })
                        .min_by(f64::total_cmp)
                });

                let min_x = min_text_x.map_or(start_x, |min_text_x| {
                    cmp::min_by(min_text_x, start_x, f64::total_cmp)
                });

                // Is start_line on screen, and is the vertical line to the left of the opening
                // bracket?
                if let Some(y) = start_line_y.filter(|_| start_x > min_x) {
                    let p0 = Point::new(min_x, y);
                    let p1 = Point::new(start_x, y);
                    let line = Line::new(p0, p1);

                    cx.stroke(&line, brush, &Stroke::new(1.0));
                }

                // Is end_line on screen, and is the vertical line to the left of the closing
                // bracket?
                if let Some(y) = end_line_y.filter(|_| end_x > min_x) {
                    let p0 = Point::new(min_x, y);
                    let p1 = Point::new(end_x, y);
                    let line = Line::new(p0, p1);

                    cx.stroke(&line, brush, &Stroke::new(1.0));
                }

                let p0 = Point::new(min_x, y0);
                let p1 = Point::new(min_x, y1);
                let line = Line::new(p0, p1);

                cx.stroke(&line, brush, &Stroke::new(1.0));
            }
        }
    }

    /// Paint enclosing bracket highlights and scope lines if the corresponding settings are
    /// enabled.
    fn paint_bracket_highlights_scope_lines(
        &self,
        cx: &mut PaintCx,
        viewport: Rect,
        screen_lines: &ScreenLines,
    ) {
        let config = self.editor.common.config.get_untracked();

        if config.editor.highlight_matching_brackets
            || config.editor.highlight_scope_lines
        {
            let e_data = &self.editor;
            let ed = &e_data.editor;
            let offset = ed.cursor.with_untracked(|cursor| cursor.mode.offset());

            let bracket_offsets = e_data
                .doc_signal()
                .with_untracked(|doc| doc.find_enclosing_brackets(offset))
                .map(|(start, end)| [start, end]);

            let bracket_line_cols = bracket_offsets.map(|bracket_offsets| {
                bracket_offsets.map(|offset| {
                    let (rvline, col) =
                        ed.rvline_col_of_offset(offset, CursorAffinity::Forward);
                    (rvline, col)
                })
            });

            if config.editor.highlight_matching_brackets {
                self.paint_char_highlights(
                    cx,
                    screen_lines,
                    bracket_line_cols.into_iter().flatten(),
                );
            }

            if config.editor.highlight_scope_lines {
                if let Some([start_line_col, end_line_col]) = bracket_line_cols {
                    self.paint_scope_lines(
                        cx,
                        viewport,
                        screen_lines,
                        start_line_col,
                        end_line_col,
                    );
                }
            }
        }
    }
}

impl View for EditorView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn style_pass(&mut self, cx: &mut StyleCx<'_>) {
        let editor = &self.editor.editor;
        if editor.es.try_update(|s| s.read(cx)).unwrap() {
            editor.floem_style_id.update(|val| *val += 1);
            cx.app_state_mut().request_paint(self.id());
        }
    }

    fn debug_name(&self) -> std::borrow::Cow<'static, str> {
        "Editor View".into()
    }

    fn update(
        &mut self,
        _cx: &mut floem::context::UpdateCx,
        state: Box<dyn std::any::Any>,
    ) {
        if let Ok(state) = state.downcast() {
            self.editor.sticky_header_info.set(*state);
            self.id.request_layout();
        }
    }

    /// Compute the layout size of the editor content. The width is determined by the
    /// widest text line (plus padding), and the height by the total number of visual
    /// lines times line height. For non-local editors, size is clamped to at least
    /// the viewport size so the scroll container always fills its parent.
    ///
    /// The `text_layout(line)` call for each visible line is a critical side effect:
    /// it populates the text layout cache, which is needed for `max_line_width()` to
    /// return an accurate value. Without this, the horizontal scrollbar extent would
    /// be wrong on first render.
    fn layout(
        &mut self,
        cx: &mut floem::context::LayoutCx,
    ) -> floem::taffy::prelude::NodeId {
        cx.layout_node(self.id, true, |_cx| {
            if self.inner_node.is_none() {
                self.inner_node = Some(self.id.new_taffy_node());
            }

            let e_data = &self.editor;
            let editor = &e_data.editor;

            let viewport_size = self.viewport.get_untracked().size();

            let screen_lines = e_data.screen_lines().get_untracked();
            for (line, _) in screen_lines.iter_lines_y() {
                // Populate text layout cache so that max_line_width() is correct.
                editor.text_layout(line);
            }

            let inner_node = self.inner_node.unwrap();

            let config = self.editor.common.config.get_untracked();
            let line_height = config.editor.line_height() as f64;

            let is_local = e_data.doc().content.with_untracked(|c| c.is_local());

            let width = editor.max_line_width() + 10.0;
            let width = if !is_local {
                width.max(viewport_size.width)
            } else {
                width
            };
            let last_vline = editor.last_vline().get();
            let last_vline = e_data.visual_line(last_vline);
            let last_line_height = line_height * (last_vline + 1) as f64;
            let height = last_line_height.max(line_height);
            let height = if !is_local {
                height.max(viewport_size.height)
            } else {
                height
            };

            let margin_bottom = if !is_local
                && editor
                    .es
                    .with_untracked(EditorStyle::scroll_beyond_last_line)
            {
                line_height * 5.0
            } else {
                0.0
            };

            let style = Style::new()
                .width(width)
                .height(height)
                .margin_bottom(margin_bottom)
                .to_taffy_style();
            self.id.set_taffy_style(inner_node, style);

            vec![inner_node]
        })
    }

    fn compute_layout(
        &mut self,
        cx: &mut floem::context::ComputeLayoutCx,
    ) -> Option<Rect> {
        let viewport = cx.current_viewport();
        if self.viewport.with_untracked(|v| v != &viewport) {
            self.viewport.set(viewport);
        }
        None
    }

    /// The main paint entry point. Rendering order matters for correct layering:
    /// 1. Current line highlight (background)
    /// 2. Selection rectangles (background)
    /// 3. Find/search result outlines
    /// 4. Bracket highlights and scope lines
    /// 5. Text content (foreground, including phantom text like inlay hints)
    /// 6. Sticky headers (painted on top of scrolled content)
    /// 7. Scroll bar
    ///
    /// `screen_lines` is re-fetched between paint stages as a safety measure:
    /// some paint functions could theoretically trigger signal updates that
    /// invalidate the cached screen lines.
    fn paint(&mut self, cx: &mut PaintCx) {
        let viewport = self.viewport.get_untracked();
        let e_data = &self.editor;
        let ed = &e_data.editor;
        let config = e_data.common.config.get_untracked();
        let doc = e_data.doc();
        let is_local = doc.content.with_untracked(|content| content.is_local());
        let find_focus = self.editor.find_focus;
        let is_active =
            self.is_active.get_untracked() && !find_focus.get_untracked();
        // TODO: One way to get around the above issue would be to more careful, since we
        // technically don't need to stop it from *recomputing* just stop any possible changes, but
        // avoiding recomputation seems easiest/clearest.
        // I expect that most/all of the paint functions could restrict themselves to only what is
        // within the active screen lines without issue.
        let screen_lines = ed.screen_lines.get_untracked();
        self.paint_current_line(cx, is_local, &screen_lines);
        FloemEditorView::paint_selection(cx, ed, &screen_lines);
        self.paint_find(cx, &screen_lines);
        self.paint_bracket_highlights_scope_lines(cx, viewport, &screen_lines);
        FloemEditorView::paint_text(
            cx,
            None,
            ed,
            viewport,
            is_active,
            &screen_lines,
        );
        self.paint_sticky_headers(cx, viewport, &screen_lines);
        self.paint_scroll_bar(cx, viewport, is_local, config);
    }
}

/// Compute which lines should appear as sticky headers at the top of the editor.
/// The algorithm walks the syntax tree to find enclosing scope headers (function
/// definitions, class declarations, etc.) for the first visible line. It then
/// checks if the next line's headers differ, which determines whether the last
/// sticky header should scroll out (creating a push-up animation effect when
/// scrolling past a scope boundary).
fn get_sticky_header_info(
    editor_data: &EditorData,
    viewport: RwSignal<Rect>,
    sticky_header_height_signal: RwSignal<f64>,
    config: &LapceConfig,
) -> StickyHeaderInfo {
    let editor = &editor_data.editor;
    let doc = editor_data.doc();

    let viewport = viewport.get();
    // TODO(minor): should this be a `get`
    let screen_lines = editor.screen_lines.get();
    let line_height = config.editor.line_height() as f64;
    // let start_line = (viewport.y0 / line_height).floor() as usize;
    let Some(start) = screen_lines.lines.first() else {
        return StickyHeaderInfo {
            sticky_lines: Vec::new(),
            last_sticky_should_scroll: false,
            y_diff: 0.0,
        };
    };
    let start_info = screen_lines.info(*start).unwrap();
    let start_line = start_info.vline_info.rvline.line;

    let y_diff = viewport.y0 - start_info.vline_y;

    let mut last_sticky_should_scroll = false;
    let mut sticky_lines = Vec::new();
    if let Some(lines) = doc.sticky_headers(start_line) {
        let total_lines = lines.len();
        if total_lines > 0 {
            let line = start_line + total_lines;
            if let Some(new_lines) = doc.sticky_headers(line) {
                if new_lines.len() > total_lines {
                    sticky_lines = new_lines;
                } else {
                    sticky_lines = lines;
                    last_sticky_should_scroll = new_lines.len() < total_lines;
                    if new_lines.len() < total_lines {
                        if let Some(new_new_lines) =
                            doc.sticky_headers(start_line + total_lines - 1)
                        {
                            if new_new_lines.len() < total_lines {
                                sticky_lines.pop();
                                last_sticky_should_scroll = false;
                            }
                        } else {
                            sticky_lines.pop();
                            last_sticky_should_scroll = false;
                        }
                    }
                }
            } else {
                sticky_lines = lines;
                last_sticky_should_scroll = true;
            }
        }
    }

    let total_sticky_lines = sticky_lines.len();

    let paint_last_line = total_sticky_lines > 0
        && (last_sticky_should_scroll
            || y_diff != 0.0
            || start_line + total_sticky_lines - 1 != *sticky_lines.last().unwrap());

    // Fix up the line count in case we don't need to paint the last one.
    let total_sticky_lines = if paint_last_line {
        total_sticky_lines
    } else {
        total_sticky_lines.saturating_sub(1)
    };

    if total_sticky_lines == 0 {
        sticky_header_height_signal.set(0.0);
        return StickyHeaderInfo {
            sticky_lines: Vec::new(),
            last_sticky_should_scroll: false,
            y_diff: 0.0,
        };
    }

    let scroll_offset = if last_sticky_should_scroll {
        y_diff
    } else {
        0.0
    };

    let sticky_header_height = sticky_lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            // TODO(question): won't y_diff always be scroll_offset here? so we should just sub on
            // the outside
            let y_diff = if i == total_sticky_lines - 1 {
                scroll_offset
            } else {
                0.0
            };

            let layout = editor.text_layout(*line);
            layout.line_count() as f64 * line_height - y_diff
        })
        .sum();

    sticky_header_height_signal.set(sticky_header_height);
    StickyHeaderInfo {
        sticky_lines,
        last_sticky_should_scroll,
        y_diff,
    }
}

pub fn editor_container_view(
    workspace_data: Rc<WorkspaceData>,
    workspace: Arc<LapceWorkspace>,
    is_active: impl Fn(bool) -> bool + 'static + Copy,
    editor: RwSignal<EditorData>,
) -> impl View {
    let (editor_id, find_focus, sticky_header_height, editor_view, config, doc, ed) =
        editor.with_untracked(|editor| {
            (
                editor.id(),
                editor.find_focus,
                editor.sticky_header_height,
                editor.kind,
                editor.common.config,
                editor.doc_signal(),
                editor.editor.clone(),
            )
        });

    let main_split = workspace_data.main_split.clone();
    let editors = main_split.editors;
    let scratch_docs = main_split.scratch_docs;

    // Create per-editor find/replace editors
    let editor_scope = editor.with_untracked(|ed| ed.scope);
    let common = main_split.common.clone();
    let find_ed = editors.make_local(editor_scope, common.clone());
    let replace_ed = editors.make_local(editor_scope, common);

    // Store them on the EditorData so command dispatch can reach them
    editor.with_untracked(|ed_data| {
        ed_data.find_editor_signal.set(Some(find_ed.clone()));
        ed_data.replace_editor_signal.set(Some(replace_ed.clone()));
    });

    // Sync find editor text -> editor's Find state
    {
        let find_ed_buf = find_ed.doc().buffer;
        let find = editor.with_untracked(|ed| ed.find.clone());
        create_effect(move |_| {
            let content = find_ed_buf.with(|buffer| buffer.to_string());
            find.set_find(&content);
        });
    }

    let replace_active = editor.with_untracked(|ed| ed.find.replace_active);
    let replace_focus = editor.with_untracked(|ed| ed.find.replace_focus);

    let viewport = ed.viewport;
    let screen_lines = ed.screen_lines;

    stack((
        editor_breadcrumbs(workspace, editor.get_untracked(), config),
        stack((
            editor_gutter(workspace_data.clone(), editor),
            editor_gutter_folding_range(
                workspace_data.clone(),
                doc,
                screen_lines,
                viewport,
            ),
            editor_content(editor, is_active),
            empty().style(move |s| {
                let config = config.get();
                s.absolute()
                    .width_pct(100.0)
                    .height(sticky_header_height.get() as f32)
                    .apply_if(
                        !config.editor.sticky_header
                            || sticky_header_height.get() == 0.0
                            || !editor_view.get().is_normal(),
                        |s| s.hide(),
                    )
            }),
            find_view(
                editor,
                find_ed,
                find_focus,
                replace_ed,
                replace_active,
                replace_focus,
                is_active,
                editor_view,
            )
            .debug_name("find view"),
        ))
        .style(|s| s.width_full().flex_basis(0).flex_grow(1.0)),
    ))
    .on_cleanup(move || {
        let editor = editor.get_untracked();
        editor.cancel_completion();
        editor.cancel_inline_completion();
        // Clear find/replace editor refs
        editor.find_editor_signal.set(None);
        editor.replace_editor_signal.set(None);
        if editors.contains_untracked(editor_id) {
            // editor still exist, so it might be moved to a different editor tab
            return;
        }
        let doc = editor.doc();
        editor.scope.dispose();

        let scratch_doc_name =
            if let DocContent::Scratch { name, .. } = doc.content.get_untracked() {
                Some(name.to_string())
            } else {
                None
            };
        if let Some(name) = scratch_doc_name {
            if !scratch_docs
                .with_untracked(|scratch_docs| scratch_docs.contains_key(&name))
            {
                doc.scope.dispose();
            }
        }
    })
    .style(|s| s.flex_col().absolute().size_pct(100.0, 100.0))
    .debug_name("Editor Container")
}

fn editor_gutter_code_lens_view(
    workspace_data: Rc<WorkspaceData>,
    line: usize,
    lens: (PluginId, usize, im::Vector<CodeLens>),
    screen_lines: RwSignal<ScreenLines>,
    viewport: RwSignal<Rect>,
    icon_padding: f32,
) -> impl View {
    let config = workspace_data.common.config;
    let view = container(svg(move || config.get().ui_svg(LapceIcons::START)).style(
        move |s| {
            let config = config.get();
            let size = config.ui.icon_size() as f32;
            s.size(size, size)
                .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
        },
    ))
    .style(move |s| {
        let config = config.get();
        s.padding(4.0)
            .border_radius(LapceLayout::BORDER_RADIUS)
            .hover(|s| {
                s.cursor(CursorStyle::Pointer)
                    .background(config.color(LapceColor::PANEL_HOVERED_BACKGROUND))
            })
            .active(|s| {
                s.background(
                    config.color(LapceColor::PANEL_HOVERED_ACTIVE_BACKGROUND),
                )
            })
    })
    .on_click_stop({
        move |_| {
            let (plugin_id, offset, lens) = lens.clone();
            workspace_data.show_code_lens(true, plugin_id, offset, lens);
        }
    });
    container(view).style(move |s| {
        let line_info = screen_lines.with(|s| s.info_for_line(line));
        let line_y = line_info.clone().map(|l| l.y).unwrap_or(-100.0);
        let rect = viewport.get();
        let config = config.get();
        let icon_size = config.ui.icon_size();
        let width = icon_size as f32 + icon_padding * 2.0;
        s.absolute()
            .width(width)
            .height(config.editor.line_height() as f32)
            .justify_center()
            .items_center()
            .margin_top(line_y as f32 - rect.y0 as f32)
    })
}

fn editor_gutter_folding_view(
    workspace_data: Rc<WorkspaceData>,
    screen_lines: RwSignal<ScreenLines>,
    viewport: RwSignal<Rect>,
    folding_display_item: FoldingDisplayItem,
) -> impl View {
    let config = workspace_data.common.config;
    let view = container(
        svg(move || {
            let icon_str = match folding_display_item {
                FoldingDisplayItem::UnfoldStart(_) => LapceIcons::FOLD_DOWN,
                FoldingDisplayItem::Folded(_) => LapceIcons::FOLD,
                FoldingDisplayItem::UnfoldEnd(_) => LapceIcons::FOLD_UP,
            };
            config.get().ui_svg(icon_str)
        })
        .style(move |s| {
            let config = config.get();
            let size = config.ui.icon_size() as f32;
            s.size(size, size)
                .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
        }),
    )
    .style(move |s| {
        let config = config.get();
        s.padding(4.0)
            .border_radius(LapceLayout::BORDER_RADIUS)
            .hover(|s| {
                s.cursor(CursorStyle::Pointer)
                    .background(config.color(LapceColor::PANEL_HOVERED_BACKGROUND))
            })
            .active(|s| {
                s.background(
                    config.color(LapceColor::PANEL_HOVERED_ACTIVE_BACKGROUND),
                )
            })
    });
    container(view).style(move |s| {
        let line = folding_display_item.position().line;
        let line_info = screen_lines.with(|s| s.info_for_line(line as usize));
        let line_y = line_info.clone().map(|l| l.y).unwrap_or(-100.0);
        let rect = viewport.get();
        let config = config.get();
        let icon_size = config.ui.icon_size();
        let width = icon_size as f32 + 4.0;
        s.absolute()
            .width(width / 2.0)
            .height(config.editor.line_height() as f32)
            .justify_center()
            .items_center()
            .margin_top(line_y as f32 - rect.y0 as f32)
    })
}

fn editor_gutter_code_lens(
    workspace_data: Rc<WorkspaceData>,
    doc: DocSignal,
    screen_lines: RwSignal<ScreenLines>,
    viewport: RwSignal<Rect>,
    icon_padding: f32,
) -> impl View {
    let config = workspace_data.common.config;

    dyn_stack(
        move || {
            let doc = doc.get();
            doc.code_lens.get()
        },
        move |(line, _)| (*line, doc.with_untracked(|doc| doc.rev())),
        move |(line, lens)| {
            editor_gutter_code_lens_view(
                workspace_data.clone(),
                line,
                lens,
                screen_lines,
                viewport,
                icon_padding,
            )
        },
    )
    .style(move |s| {
        let config = config.get();
        let width = config.ui.icon_size() as f32 + icon_padding * 2.0;
        s.absolute()
            .width(width)
            .height_full()
            .margin_left(width - 8.0)
    })
    .debug_name("CodeLens Stack")
}

fn editor_gutter_folding_range(
    workspace_data: Rc<WorkspaceData>,
    doc: DocSignal,
    screen_lines: RwSignal<ScreenLines>,
    viewport: RwSignal<Rect>,
) -> impl View {
    let config = workspace_data.common.config;
    dyn_stack(
        move || doc.get().folding_ranges.get().to_display_items(),
        move |item| *item,
        move |item| {
            editor_gutter_folding_view(
                workspace_data.clone(),
                screen_lines,
                viewport,
                item,
            )
            .on_click_stop({
                move |_| {
                    doc.get_untracked().folding_ranges.update(|x| match item {
                        FoldingDisplayItem::UnfoldStart(pos)
                        | FoldingDisplayItem::Folded(pos) => {
                            x.0.iter_mut().find_map(|mut range| {
                                let range = range.deref_mut();
                                if range.start == pos {
                                    range.status.click();
                                    Some(())
                                } else {
                                    None
                                }
                            });
                        }
                        FoldingDisplayItem::UnfoldEnd(pos) => {
                            x.0.iter_mut().find_map(|mut range| {
                                let range = range.deref_mut();
                                if range.end == pos {
                                    range.status.click();
                                    Some(())
                                } else {
                                    None
                                }
                            });
                        }
                    })
                }
            })
        },
    )
    .style(move |s| {
        let config = config.get();
        let width = config.ui.icon_size() as f32;
        // hide for now
        s.width(width)
            .height_full()
            .margin_left(-width / 2.0)
            .hide()
    })
    .debug_name("Folding Range Stack")
}

fn editor_gutter_code_actions(
    e_data: RwSignal<EditorData>,
    gutter_width: Memo<f64>,
    icon_padding: f32,
) -> impl View {
    let (ed, doc, config) = e_data
        .with_untracked(|e| (e.editor.clone(), e.doc_signal(), e.common.config));
    let viewport = ed.viewport;
    let cursor = ed.cursor;

    let code_action_vline = create_memo(move |_| {
        let doc = doc.get();
        let (offset, affinity) =
            cursor.with(|cursor| (cursor.offset(), cursor.affinity));
        let has_code_actions = doc
            .code_actions()
            .with(|c| c.get(&offset).map(|c| !c.1.is_empty()).unwrap_or(false));
        if has_code_actions {
            let vline = ed.vline_of_offset(offset, affinity);
            Some(vline)
        } else {
            None
        }
    });

    container(
        container(
            svg(move || config.get().ui_svg(LapceIcons::LIGHTBULB)).style(
                move |s| {
                    let config = config.get();
                    let size = config.ui.icon_size() as f32;
                    s.size(size, size)
                        .color(config.color(LapceColor::LAPCE_WARN))
                },
            ),
        )
        .on_click_stop(move |_| {
            e_data.get_untracked().show_code_actions(true);
        })
        .style(move |s| {
            let config = config.get();
            s.padding(4.0)
                .border_radius(LapceLayout::BORDER_RADIUS)
                .hover(|s| {
                    s.cursor(CursorStyle::Pointer).background(
                        config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                    )
                })
                .active(|s| {
                    s.background(
                        config.color(LapceColor::PANEL_HOVERED_ACTIVE_BACKGROUND),
                    )
                })
        }),
    )
    .style(move |s| {
        let config = config.get();
        let viewport = viewport.get();
        let gutter_width = gutter_width.get();
        let code_action_vline = code_action_vline.get();
        let size = config.ui.icon_size() as f32;
        let line_height = config.editor.line_height();
        let margin_top = if let Some(vline) = code_action_vline {
            (vline.get() * line_height) as f32 - viewport.y0 as f32
        } else {
            0.0
        };
        let width = size + icon_padding * 2.0;
        s.absolute()
            .items_center()
            .justify_center()
            .margin_left(gutter_width as f32 - width + 1.0)
            .margin_top(margin_top)
            .width(width)
            .height(line_height as f32)
            .apply_if(code_action_vline.is_none(), |s| s.hide())
    })
    .debug_name("Code Action LightBulb")
}

fn editor_gutter(
    workspace_data: Rc<WorkspaceData>,
    e_data: RwSignal<EditorData>,
) -> impl View {
    let icon_padding = 6.0;

    let (ed, doc, config) = e_data
        .with_untracked(|e| (e.editor.clone(), e.doc_signal(), e.common.config));
    let viewport = ed.viewport;
    let scroll_delta = ed.scroll_delta;
    let screen_lines = ed.screen_lines;

    let gutter_rect = create_rw_signal(Rect::ZERO);
    let gutter_width = create_memo(move |_| gutter_rect.get().width());

    let icon_total_width = move || {
        let icon_size = config.get().ui.icon_size() as f32;
        icon_size + icon_padding * 2.0
    };

    let gutter_padding_right = create_memo(move |_| icon_total_width() + 6.0);

    stack((
        stack((
            empty().style(move |s| s.width(icon_total_width() * 2.0 - 8.0)),
            label(move || {
                let doc = doc.get();
                doc.buffer.with(|b| b.last_line() + 1).to_string()
            })
            .style(|s| s.color(Color::TRANSPARENT)),
            empty().style(move |s| s.width(gutter_padding_right.get())),
        ))
        .debug_name("Centered Last Line Count")
        .style(|s| s.height_pct(100.0)),
        clip(
            stack((
                editor_gutter_code_lens(
                    workspace_data.clone(),
                    doc,
                    screen_lines,
                    viewport,
                    icon_padding,
                ),
                editor_gutter_view(e_data.get_untracked(), gutter_padding_right)
                    .on_resize(move |rect| {
                        gutter_rect.set(rect);
                    })
                    .on_event_stop(EventListener::PointerWheel, move |event| {
                        if let Event::PointerWheel(pointer_event) = event {
                            scroll_delta.set(pointer_event.delta);
                        }
                    })
                    .style(|s| s.size_pct(100.0, 100.0)),
                editor_gutter_code_actions(e_data, gutter_width, icon_padding),
            ))
            .style(|s| s.size_pct(100.0, 100.0)),
        )
        .style(move |s| s.absolute().size_pct(100.0, 100.0)),
    ))
    .style(|s| s.height_pct(100.0))
    .debug_name("Editor Gutter")
}

fn editor_breadcrumbs(
    workspace: Arc<LapceWorkspace>,
    e_data: EditorData,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let doc = e_data.doc_signal();
    let doc_path = create_memo(move |_| {
        let doc = doc.get();
        let content = doc.content.get();
        if let DocContent::History(history) = &content {
            Some(history.path.clone())
        } else {
            content.path().cloned()
        }
    });
    container(
        scroll(
            stack((
                {
                    let workspace = workspace.clone();
                    dyn_stack(
                        move || {
                            let full_path = doc_path.get().unwrap_or_default();
                            let mut path = full_path;
                            if let Some(workspace_path) =
                                workspace.clone().path.as_ref()
                            {
                                path = path
                                    .strip_prefix(workspace_path)
                                    .unwrap_or(&path)
                                    .to_path_buf();
                            }
                            path.ancestors()
                                .filter_map(|path| {
                                    Some(
                                        path.file_name()?
                                            .to_string_lossy()
                                            .into_owned(),
                                    )
                                })
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .enumerate()
                        },
                        |(i, section)| (*i, section.to_string()),
                        move |(i, section)| {
                            stack((
                                svg(move || {
                                    config
                                        .get()
                                        .ui_svg(LapceIcons::BREADCRUMB_SEPARATOR)
                                })
                                .style(move |s| {
                                    let config = config.get();
                                    let size = config.ui.icon_size() as f32;
                                    s.apply_if(i == 0, |s| s.hide())
                                        .size(size, size)
                                        .color(
                                            config.color(
                                                LapceColor::LAPCE_ICON_ACTIVE,
                                            ),
                                        )
                                }),
                                label(move || section.clone())
                                    .style(move |s| s.selectable(false)),
                            ))
                            .style(|s| s.items_center())
                        },
                    )
                    .style(|s| s.padding_horiz(10.0))
                },
                label(move || {
                    let doc = doc.get();
                    if let DocContent::History(history) = doc.content.get() {
                        format!("({})", history.version)
                    } else {
                        "".to_string()
                    }
                })
                .style(move |s| {
                    let doc = doc.get();
                    let is_history = doc.content.with_untracked(|content| {
                        matches!(content, DocContent::History(_))
                    });

                    s.padding_right(10.0).apply_if(!is_history, |s| s.hide())
                }),
            ))
            .style(|s| s.items_center()),
        )
        .scroll_to(move || {
            doc.track();
            Some(Point::new(3000.0, 0.0))
        })
        .scroll_style(|s| s.hide_bars(true))
        .style(move |s| {
            s.absolute()
                .size_pct(100.0, 100.0)
                .border_bottom(1.0)
                .border_color(config.get().color(LapceColor::LAPCE_BORDER))
                .items_center()
        }),
    )
    .style(move |s| {
        let config = config.get_untracked();
        let line_height = config.editor.line_height();
        s.items_center()
            .width_pct(100.0)
            .height(line_height as f32)
            .apply_if(doc_path.get().is_none(), |s| s.hide())
            .apply_if(!config.editor.show_bread_crumbs, |s| s.hide())
    })
    .debug_name("Editor BreadCrumbs")
}

fn editor_content(
    e_data: RwSignal<EditorData>,
    is_active: impl Fn(bool) -> bool + 'static + Copy,
) -> impl View {
    let (
        cursor,
        scroll_delta,
        scroll_to,
        window_origin,
        viewport,
        sticky_header_height,
        config,
        editor,
    ) = e_data.with_untracked(|editor| {
        (
            editor.cursor().read_only(),
            editor.scroll_delta().read_only(),
            editor.scroll_to(),
            editor.window_origin(),
            editor.viewport(),
            editor.sticky_header_height,
            editor.common.config,
            editor.editor.clone(),
        )
    });

    {
        create_effect(move |_| {
            is_active(true);
            let e_data = e_data.get_untracked();
            e_data.cancel_completion();
            e_data.cancel_inline_completion();
        });
    }

    let current_scroll = create_rw_signal(Rect::ZERO);

    scroll({
        let editor_content_view = editor_view(e_data.get_untracked(), is_active)
            .style(move |s| {
                s.absolute()
                    .margin_left(1.0)
                    .min_size_full()
                    .cursor(CursorStyle::Text)
            });

        let id = editor_content_view.id();
        editor.editor_view_id.set(Some(id));

        let editor2 = editor.clone();
        editor_content_view
            .on_event_cont(EventListener::FocusGained, move |_| {
                editor.editor_view_focused.notify();
            })
            .on_event_cont(EventListener::FocusLost, move |_| {
                editor2.editor_view_focus_lost.notify();
            })
            .on_event_cont(EventListener::PointerDown, move |event| {
                if let Event::PointerDown(pointer_event) = event {
                    id.request_active();
                    e_data.get_untracked().pointer_down(pointer_event);
                }
            })
            .on_event_stop(EventListener::PointerMove, move |event| {
                if let Event::PointerMove(pointer_event) = event {
                    e_data.get_untracked().pointer_move(pointer_event);
                }
            })
            .on_event_stop(EventListener::PointerUp, move |event| {
                if let Event::PointerUp(pointer_event) = event {
                    e_data.get_untracked().pointer_up(pointer_event);
                }
            })
            .on_event_stop(EventListener::PointerLeave, move |event| {
                if let Event::PointerLeave = event {
                    e_data.get_untracked().pointer_leave();
                }
            })
    })
    .on_move(move |point| {
        window_origin.set(point);
    })
    .on_scroll(move |rect| {
        if rect.y0 != current_scroll.get_untracked().y0 {
            // only cancel completion if scrolled vertically
            let e_data = e_data.get_untracked();
            e_data.cancel_completion();
            e_data.cancel_inline_completion();
        }
        current_scroll.set(rect);
    })
    .scroll_to(move || scroll_to.get().map(|s| s.to_point()))
    .scroll_delta(move || scroll_delta.get())
    // Ensure the cursor is visible within the scroll viewport. When the cursor
    // is very far from the viewport (e.g., after a goto-line), we inflate the
    // rect to center the view rather than scrolling to the edge. The
    // `cursor_surrounding_lines` config adds padding above/below the cursor,
    // and `sticky_header_height` is subtracted from the top padding so the
    // cursor isn't hidden behind sticky headers.
    .ensure_visible(move || {
        let e_data = e_data.get_untracked();
        let cursor = cursor.get();
        let offset = cursor.offset();
        e_data.doc_signal().track();
        e_data.kind.track();

        let LineRegion { x, width, rvline } = cursor_caret(
            &e_data.editor,
            offset,
            !cursor.is_insert(),
            cursor.affinity,
        );
        let config = config.get_untracked();
        let line_height = config.editor.line_height();
        // TODO: is there a good way to avoid the calculation of the vline here?
        let vline = e_data.editor.vline_of_rvline(rvline);
        let vline = e_data.visual_line(vline.get());
        let rect = Rect::from_origin_size(
            (x, (vline * line_height) as f64),
            (width, line_height as f64),
        )
        .inflate(10.0, 0.0);

        let viewport = viewport.get_untracked();
        let smallest_distance = (viewport.y0 - rect.y0)
            .abs()
            .min((viewport.y1 - rect.y0).abs())
            .min((viewport.y0 - rect.y1).abs())
            .min((viewport.y1 - rect.y1).abs());
        let biggest_distance = (viewport.y0 - rect.y0)
            .abs()
            .max((viewport.y1 - rect.y0).abs())
            .max((viewport.y0 - rect.y1).abs())
            .max((viewport.y1 - rect.y1).abs());
        let jump_to_middle = biggest_distance > viewport.height()
            && smallest_distance > viewport.height() / 2.0;

        if jump_to_middle {
            rect.inflate(0.0, viewport.height() / 2.0)
        } else {
            let cursor_surrounding_lines =
                e_data.editor.es.with(|s| s.cursor_surrounding_lines());
            let mut rect = rect;
            rect.y0 -= (cursor_surrounding_lines * line_height) as f64
                + sticky_header_height.get_untracked();
            rect.y1 += (cursor_surrounding_lines * line_height) as f64;
            rect
        }
    })
    .style(|s| s.size_full().set(PropagatePointerWheel, false))
    .debug_name("Editor Content")
}

fn search_editor_view(
    find_editor: EditorData,
    find_focus: RwSignal<bool>,
    is_active: impl Fn(bool) -> bool + 'static + Copy,
    replace_focus: RwSignal<bool>,
    find_visual: RwSignal<bool>,
    case_matching: RwSignal<CaseMatching>,
    whole_word: RwSignal<bool>,
    is_regex: RwSignal<bool>,
) -> impl View {
    let config = find_editor.common.config;
    let visual = find_visual;

    stack((
        TextInputBuilder::new()
            .is_focused(move || {
                is_active(true)
                    && visual.get()
                    && find_focus.get()
                    && !replace_focus.get()
            })
            .build_editor(find_editor)
            .on_event_cont(EventListener::PointerDown, move |_| {
                find_focus.set(true);
                replace_focus.set(false);
            })
            .style(|s| s.width_pct(100.0)),
        clickable_icon(
            || LapceIcons::SEARCH_CASE_SENSITIVE,
            move || {
                let new = match case_matching.get_untracked() {
                    CaseMatching::Exact => CaseMatching::CaseInsensitive,
                    CaseMatching::CaseInsensitive => CaseMatching::Exact,
                };
                case_matching.set(new);
            },
            move || case_matching.get() == CaseMatching::Exact,
            || false,
            || "Case Sensitive",
            config,
        )
        .style(|s| s.padding_vert(4.0)),
        clickable_icon(
            || LapceIcons::SEARCH_WHOLE_WORD,
            move || {
                whole_word.update(|whole_word| {
                    *whole_word = !*whole_word;
                });
            },
            move || whole_word.get(),
            || false,
            || "Whole Word",
            config,
        )
        .style(|s| s.padding_left(6.0)),
        clickable_icon(
            || LapceIcons::SEARCH_REGEX,
            move || {
                is_regex.update(|is_regex| {
                    *is_regex = !*is_regex;
                });
            },
            move || is_regex.get(),
            || false,
            || "Use Regex",
            config,
        )
        .style(|s| s.padding_horiz(6.0)),
    ))
    .style(move |s| {
        let config = config.get();
        s.width(200.0)
            .items_center()
            .border(1.0)
            .border_radius(LapceLayout::BORDER_RADIUS)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
    })
}

fn replace_editor_view(
    replace_editor: EditorData,
    replace_active: RwSignal<bool>,
    replace_focus: RwSignal<bool>,
    is_active: impl Fn(bool) -> bool + 'static + Copy,
    find_focus: RwSignal<bool>,
    find_visual: RwSignal<bool>,
) -> impl View {
    let config = replace_editor.common.config;
    let visual = find_visual;

    stack((
        TextInputBuilder::new()
            .is_focused(move || {
                is_active(true)
                    && visual.get()
                    && find_focus.get()
                    && replace_active.get()
                    && replace_focus.get()
            })
            .build_editor(replace_editor)
            .on_event_cont(EventListener::PointerDown, move |_| {
                find_focus.set(true);
                replace_focus.set(true);
            })
            .style(|s| s.width_pct(100.0)),
        empty().style(move |s| {
            let config = config.get();
            let size = config.ui.icon_size() as f32 + 10.0;
            s.size(0.0, size).padding_vert(4.0)
        }),
    ))
    .style(move |s| {
        let config = config.get();
        s.width(200.0)
            .items_center()
            .border(1.0)
            .border_radius(LapceLayout::BORDER_RADIUS)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
    })
}

fn replace_button(
    icon: &'static str,
    text: &'static str,
    on_click: impl Fn() + 'static,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    stack((
        svg(move || config.get().ui_svg(icon)).style(move |s| {
            let config = config.get();
            let size = config.ui.icon_size() as f32;
            s.size(size, size)
                .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
        }),
        label(move || text).style(move |s| {
            let config = config.get();
            s.margin_left(4.0)
                .font_size(config.ui.font_size() as f32 - 1.0)
                .selectable(false)
                .color(config.color(LapceColor::EDITOR_FOREGROUND))
        }),
    ))
    .style(move |s| {
        let config = config.get();
        s.items_center()
            .padding_horiz(6.0)
            .padding_vert(2.0)
            .border_radius(LapceLayout::BORDER_RADIUS)
            .border(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .hover(|s| {
                s.cursor(CursorStyle::Pointer)
                    .background(config.color(LapceColor::PANEL_HOVERED_BACKGROUND))
            })
            .active(|s| {
                s.background(
                    config.color(LapceColor::PANEL_HOVERED_ACTIVE_BACKGROUND),
                )
            })
    })
    .on_click_stop(move |_| {
        on_click();
    })
}

fn find_view(
    editor: RwSignal<EditorData>,
    find_editor: EditorData,
    find_focus: RwSignal<bool>,
    replace_editor: EditorData,
    replace_active: RwSignal<bool>,
    replace_focus: RwSignal<bool>,
    is_active: impl Fn(bool) -> bool + 'static + Copy,
    editor_view: RwSignal<EditorViewKind>,
) -> impl View {
    let common = find_editor.common.clone();
    let config = common.config;
    let find_visual = editor.with_untracked(|ed| ed.find.visual);
    let case_matching = editor.with_untracked(|ed| ed.find.case_matching);
    let whole_word = editor.with_untracked(|ed| ed.find.whole_words);
    let is_regex = editor.with_untracked(|ed| ed.find.is_regex);
    let find_result_occurrences =
        editor.with_untracked(|ed| ed.find_result.occurrences);
    let replace_doc = replace_editor.doc_signal();
    let focus = common.focus;

    let find_pos = create_memo(move |_| {
        let visual = find_visual.get();
        if !visual {
            return (0, 0);
        }
        let editor = editor.get_untracked();
        let cursor = editor.cursor();
        let offset = cursor.with(|cursor| cursor.offset());
        find_result_occurrences.with(|occurrences| {
            for (i, region) in occurrences.regions().iter().enumerate() {
                if offset <= region.max() {
                    return (i + 1, occurrences.regions().len());
                }
            }
            (occurrences.regions().len(), occurrences.regions().len())
        })
    });

    container(
        stack((
            stack((
                search_editor_view(
                    find_editor,
                    find_focus,
                    is_active,
                    replace_focus,
                    find_visual,
                    case_matching,
                    whole_word,
                    is_regex,
                ),
                label(move || {
                    let (current, all) = find_pos.get();
                    if all == 0 {
                        "No Results".to_string()
                    } else {
                        format!("{current} of {all}")
                    }
                })
                .style(|s| s.margin_left(6.0).min_width(70.0)),
                clickable_icon(
                    || LapceIcons::SEARCH_BACKWARD,
                    move || {
                        editor.get_untracked().search_backward(Modifiers::empty());
                    },
                    move || false,
                    || false,
                    || "Previous Match",
                    config,
                )
                .style(|s| s.padding_left(6.0)),
                clickable_icon(
                    || LapceIcons::SEARCH_FORWARD,
                    move || {
                        editor.get_untracked().search_forward(Modifiers::empty());
                    },
                    move || false,
                    || false,
                    || "Next Match",
                    config,
                )
                .style(|s| s.padding_left(6.0)),
                clickable_icon(
                    || LapceIcons::CLOSE,
                    move || {
                        editor.get_untracked().clear_search();
                    },
                    move || false,
                    || false,
                    || "Close",
                    config,
                )
                .style(|s| s.padding_horiz(6.0)),
            ))
            .style(|s| s.items_center()),
            stack((
                replace_editor_view(
                    replace_editor,
                    replace_active,
                    replace_focus,
                    is_active,
                    find_focus,
                    find_visual,
                ),
                replace_button(
                    LapceIcons::SEARCH_REPLACE,
                    "Replace",
                    move || {
                        let text = replace_doc
                            .get_untracked()
                            .buffer
                            .with_untracked(|b| b.to_string());
                        let ed = editor.get_untracked();
                        ed.replace_next(&text);
                        ed.search_forward(Modifiers::empty());
                    },
                    config,
                )
                .style(|s| s.padding_left(6.0)),
                replace_button(
                    LapceIcons::SEARCH_REPLACE_ALL,
                    "Replace All",
                    move || {
                        let text = replace_doc
                            .get_untracked()
                            .buffer
                            .with_untracked(|b| b.to_string());
                        editor.get_untracked().replace_all(&text);
                    },
                    config,
                )
                .style(|s| s.padding_left(6.0)),
            ))
            .style(move |s| {
                s.items_center()
                    .margin_top(4.0)
                    .apply_if(!replace_active.get(), |s| s.hide())
            }),
        ))
        .style(move |s| {
            let config = config.get();
            s.margin_right(50.0)
                .background(config.color(LapceColor::PANEL_BACKGROUND))
                .border_radius(LapceLayout::BORDER_RADIUS)
                .border(1.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .padding_vert(4.0)
                .cursor(CursorStyle::Default)
                .flex_col()
        })
        .on_event_stop(EventListener::PointerDown, move |_| {
            let editor = editor.get_untracked();
            if let Some(editor_tab_id) = editor.editor_tab_id.get_untracked() {
                editor
                    .common
                    .internal_command
                    .send(InternalCommand::FocusEditorTab { editor_tab_id });
            }
            focus.set(Focus::Workbench);
            common
                .window_common
                .app_view_id
                .get_untracked()
                .request_focus();
        }),
    )
    .style(move |s| {
        s.absolute()
            .margin_top(-1.0)
            .width_pct(100.0)
            .justify_end()
            .apply_if(!find_visual.get() || !editor_view.get().is_normal(), |s| {
                s.hide()
            })
    })
}
