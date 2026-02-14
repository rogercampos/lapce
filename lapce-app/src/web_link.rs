use floem::{
    View,
    peniko::Color,
    style::CursorStyle,
    views::{Decorators, label},
};

use crate::{command::InternalCommand, listener::Listener};

/// A clickable text label that opens a URL in the system browser.
/// Uses InternalCommand::OpenWebUri rather than opening directly, so the
/// workspace_data handler can use the `open` crate in a centralized way.
pub fn web_link(
    text: impl Fn() -> String + 'static,
    uri: impl Fn() -> String + 'static,
    color: impl Fn() -> Color + 'static,
    internal_command: Listener<InternalCommand>,
) -> impl View {
    label(text)
        .on_click_stop(move |_| {
            internal_command.send(InternalCommand::OpenWebUri { uri: uri() });
        })
        .style(move |s| {
            s.color(color())
                .hover(move |s| s.cursor(CursorStyle::Pointer))
        })
}
