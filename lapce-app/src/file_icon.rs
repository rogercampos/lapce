use std::{path::PathBuf, sync::Arc};

use floem::{
    reactive::{ReadSignal, SignalGet},
    style::Style,
    views::{label, stack, svg, Decorators},
    IntoView, View,
};

use crate::config::{color::LapceColor, LapceConfig};

/// Creates a file type icon SVG view for the given path.
/// Returns the icon sized to `config.ui.icon_size()` with the correct file-type color.
pub fn file_icon_svg(
    config: ReadSignal<Arc<LapceConfig>>,
    path: impl Fn() -> PathBuf + 'static + Clone,
) -> impl View {
    let style_path = path.clone();
    svg(move || config.get().file_svg(&path()).0).style(move |s| {
        let config = config.get();
        let size = config.ui.icon_size() as f32;
        let color = config.file_svg(&style_path()).1;
        s.min_width(size)
            .size(size, size)
            .margin_right(6.0)
            .apply_opt(color, Style::color)
    })
}

/// Creates a horizontal stack with: file icon + filename label + dimmed folder hint.
pub fn file_icon_with_name(
    config: ReadSignal<Arc<LapceConfig>>,
    path: impl Fn() -> PathBuf + 'static + Clone,
    name: impl Fn() -> String + 'static,
    folder: impl Fn() -> String + 'static,
) -> impl IntoView {
    stack((
        file_icon_svg(config, path),
        label(name).style(move |s| {
            s.margin_right(6.0)
                .max_width_pct(100.0)
                .text_ellipsis()
        }),
        label(folder).style(move |s| {
            s.color(config.get().color(LapceColor::EDITOR_DIM))
                .min_width(0.0)
                .text_ellipsis()
        }),
    ))
    .style(|s| s.items_center().min_width(0.0))
}
