use std::sync::Arc;

use floem::{
    IntoView, View,
    peniko::Color,
    reactive::{ReadSignal, SignalGet},
    style::CursorStyle,
    views::{Decorators, container, label, svg, tooltip},
};

use crate::config::{LapceConfig, color::LapceColor};

pub fn not_clickable_icon<S: std::fmt::Display + 'static>(
    icon: impl Fn() -> &'static str + 'static,
    active_fn: impl Fn() -> bool + 'static,
    disabled_fn: impl Fn() -> bool + 'static + Copy,
    tooltip_: impl Fn() -> S + 'static + Clone,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    tooltip_label(
        config,
        clickable_icon_base(
            icon,
            None::<Box<dyn Fn()>>,
            active_fn,
            disabled_fn,
            config,
        ),
        tooltip_,
    )
    .debug_name("Not Clickable Icon")
}

pub fn clickable_icon<S: std::fmt::Display + 'static>(
    icon: impl Fn() -> &'static str + 'static,
    on_click: impl Fn() + 'static,
    active_fn: impl Fn() -> bool + 'static,
    disabled_fn: impl Fn() -> bool + 'static + Copy,
    tooltip_: impl Fn() -> S + 'static + Clone,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    tooltip_label(
        config,
        clickable_icon_base(icon, Some(on_click), active_fn, disabled_fn, config),
        tooltip_,
    )
}

pub fn clickable_icon_base(
    icon: impl Fn() -> &'static str + 'static,
    on_click: Option<impl Fn() + 'static>,
    active_fn: impl Fn() -> bool + 'static,
    disabled_fn: impl Fn() -> bool + 'static + Copy,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let view = container(
        svg(move || config.get().ui_svg(icon()))
            .style(move |s| {
                let config = config.get();
                let size = config.ui.icon_size() as f32;
                s.size(size, size)
                    .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    .disabled(|s| {
                        s.color(config.color(LapceColor::LAPCE_ICON_INACTIVE))
                            .cursor(CursorStyle::Default)
                    })
            })
            .disabled(disabled_fn),
    )
    .disabled(disabled_fn)
    .style(move |s| {
        let config = config.get();
        s.padding(4.0)
            .border_radius(6.0)
            .border(1.0)
            .border_color(Color::TRANSPARENT)
            .apply_if(active_fn(), |s| {
                s.border_color(config.color(LapceColor::EDITOR_CARET))
            })
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

    if let Some(on_click) = on_click {
        view.on_click_stop(move |_| {
            on_click();
        })
    } else {
        view
    }
}

/// A tooltip with a label inside.
/// When styling an element that has the tooltip, it will style the child rather than the tooltip
/// label.
pub fn tooltip_label<S: std::fmt::Display + 'static, V: View + 'static>(
    config: ReadSignal<Arc<LapceConfig>>,
    child: V,
    text: impl Fn() -> S + 'static + Clone,
) -> impl View {
    tooltip(child, move || {
        tooltip_tip(
            config,
            label(text.clone()).style(move |s| s.selectable(false)),
        )
    })
}

pub(crate) fn tooltip_tip<V: View + 'static>(
    config: ReadSignal<Arc<LapceConfig>>,
    child: V,
) -> impl IntoView {
    container(child).style(move |s| {
        let config = config.get();
        s.padding_horiz(10.0)
            .padding_vert(5.0)
            .font_size(config.ui.font_size() as f32)
            .font_family(config.ui.font_family.clone())
            .color(config.color(LapceColor::TOOLTIP_FOREGROUND))
            .background(config.color(LapceColor::TOOLTIP_BACKGROUND))
            .border(1)
            .border_radius(6)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .box_shadow_blur(3.0)
            .box_shadow_color(config.color(LapceColor::LAPCE_DROPDOWN_SHADOW))
            .margin_left(0.0)
            .margin_top(4.0)
    })
}
