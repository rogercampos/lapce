use floem::{
    peniko::kurbo::Rect,
    reactive::{RwSignal, Scope},
};

/// Measurements of the workspace tab's chrome, updated from floem's layout
/// pass. Consumed by popup positioning (completion, hover, code action,
/// rename, definition picker) and the title/status bars.
#[derive(Clone, Copy)]
pub struct LayoutState {
    pub rect: RwSignal<Rect>,
    pub title_height: RwSignal<f64>,
    pub status_height: RwSignal<f64>,
}

impl LayoutState {
    pub fn new(cx: Scope) -> Self {
        Self {
            rect: cx.create_rw_signal(Rect::ZERO),
            title_height: cx.create_rw_signal(0.0),
            status_height: cx.create_rw_signal(0.0),
        }
    }
}
