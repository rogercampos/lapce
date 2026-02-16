use floem::{
    IntoView, View, ViewId,
    event::{Event, EventPropagation},
    peniko::kurbo::Point,
    style::{CursorStyle, Style},
};

const EDGE_WIDTH: f64 = 10.0;

#[derive(Clone, Copy, Debug)]
enum ResizeEdge {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

struct DragState {
    edge: ResizeEdge,
    start_abs_pos: Point,
    start_width: f64,
    start_height: f64,
}

pub struct ResizableContainer {
    id: ViewId,
    cursor_style: CursorStyle,
    width: f64,
    height: f64,
    min_width: f64,
    min_height: f64,
    drag: Option<DragState>,
}

pub fn resizable_container(
    initial_width: f64,
    initial_height: f64,
    min_width: f64,
    min_height: f64,
    child: impl IntoView + 'static,
) -> ResizableContainer {
    let id = ViewId::new();
    id.set_children([child]);
    ResizableContainer {
        id,
        cursor_style: CursorStyle::Default,
        width: initial_width,
        height: initial_height,
        min_width,
        min_height,
        drag: None,
    }
}

fn detect_edge(pos: Point, width: f64, height: f64) -> Option<ResizeEdge> {
    let near_left = pos.x < EDGE_WIDTH;
    let near_right = pos.x > width - EDGE_WIDTH;
    let near_top = pos.y < EDGE_WIDTH;
    let near_bottom = pos.y > height - EDGE_WIDTH;

    if near_left && near_top {
        Some(ResizeEdge::TopLeft)
    } else if near_right && near_top {
        Some(ResizeEdge::TopRight)
    } else if near_left && near_bottom {
        Some(ResizeEdge::BottomLeft)
    } else if near_right && near_bottom {
        Some(ResizeEdge::BottomRight)
    } else if near_left {
        Some(ResizeEdge::Left)
    } else if near_right {
        Some(ResizeEdge::Right)
    } else if near_top {
        Some(ResizeEdge::Top)
    } else if near_bottom {
        Some(ResizeEdge::Bottom)
    } else {
        None
    }
}

fn cursor_for_edge(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::NwseResize,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::NeswResize,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ColResize,
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::RowResize,
    }
}

impl View for ResizableContainer {
    fn id(&self) -> ViewId {
        self.id
    }

    fn debug_name(&self) -> std::borrow::Cow<'static, str> {
        "Resizable Container".into()
    }

    fn view_style(&self) -> Option<Style> {
        Some(
            Style::new()
                .width(self.width as f32)
                .height(self.height as f32)
                .cursor(self.cursor_style),
        )
    }

    fn event_before_children(
        &mut self,
        cx: &mut floem::context::EventCx,
        event: &Event,
    ) -> EventPropagation {
        match event {
            Event::PointerMove(pointer) => {
                if cx.is_active(self.id) {
                    // Currently dragging
                    if let Some(drag) = &self.drag {
                        let abs_pos =
                            pointer.pos + self.id.layout_rect().origin().to_vec2();
                        let dx = abs_pos.x - drag.start_abs_pos.x;
                        let dy = abs_pos.y - drag.start_abs_pos.y;

                        let (dw, dh) = match drag.edge {
                            ResizeEdge::Right => (2.0 * dx, 0.0),
                            ResizeEdge::Left => (-2.0 * dx, 0.0),
                            ResizeEdge::Bottom => (0.0, 2.0 * dy),
                            ResizeEdge::Top => (0.0, -2.0 * dy),
                            ResizeEdge::BottomRight => (2.0 * dx, 2.0 * dy),
                            ResizeEdge::TopLeft => (-2.0 * dx, -2.0 * dy),
                            ResizeEdge::TopRight => (2.0 * dx, -2.0 * dy),
                            ResizeEdge::BottomLeft => (-2.0 * dx, 2.0 * dy),
                        };

                        self.width = (drag.start_width + dw).max(self.min_width);
                        self.height = (drag.start_height + dh).max(self.min_height);
                        self.id.request_style();
                        self.id.request_layout();
                    }
                    return EventPropagation::Stop;
                }

                // Not dragging — update cursor based on edge proximity
                let new_cursor = detect_edge(pointer.pos, self.width, self.height)
                    .map(cursor_for_edge)
                    .unwrap_or(CursorStyle::Default);

                if new_cursor != self.cursor_style {
                    self.cursor_style = new_cursor;
                    self.id.request_style();
                }
                EventPropagation::Continue
            }
            Event::PointerDown(pointer) => {
                if let Some(edge) = detect_edge(pointer.pos, self.width, self.height)
                {
                    let abs_pos =
                        pointer.pos + self.id.layout_rect().origin().to_vec2();
                    self.drag = Some(DragState {
                        edge,
                        start_abs_pos: abs_pos,
                        start_width: self.width,
                        start_height: self.height,
                    });
                    cx.update_active(self.id);
                    return EventPropagation::Stop;
                }
                EventPropagation::Continue
            }
            Event::PointerUp(_) => {
                if self.drag.is_some() {
                    self.drag = None;
                    self.id.clear_active();
                    return EventPropagation::Stop;
                }
                EventPropagation::Continue
            }
            _ => EventPropagation::Continue,
        }
    }

    fn paint(&mut self, cx: &mut floem::context::PaintCx) {
        cx.paint_children(self.id());
    }
}
