use std::{rc::Rc, sync::Arc};

use floem::{
    View,
    event::EventListener,
    keyboard::Modifiers,
    reactive::{ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate},
    style::{CursorStyle, Display, Position},
    views::{Decorators, container, label, stack, svg},
};
use lapce_core::{command::FocusCommand, meta::VERSION};

use crate::{
    command::{CommandExecuted, CommandKind},
    config::{LapceConfig, color::LapceColor, layout::LapceLayout},
    keypress::KeyPressFocus,
    web_link::web_link,
    workspace_data::{Focus, WorkspaceData},
};

struct AboutUri {}

impl AboutUri {
    const LAPCE: &'static str = "https://lapce.dev";
    const GITHUB: &'static str = "https://github.com/lapce/lapce";
    const MATRIX: &'static str = "https://matrix.to/#/#lapce-editor:matrix.org";
    const DISCORD: &'static str = "https://discord.gg/n8tGJ6Rn6D";
    const CODICONS: &'static str = "https://github.com/microsoft/vscode-codicons";
}

#[derive(Clone, Debug)]
pub struct AboutData {
    pub visible: RwSignal<bool>,
    pub focus: RwSignal<Focus>,
}

impl AboutData {
    pub fn new(cx: Scope, focus: RwSignal<Focus>) -> Self {
        let visible = cx.create_rw_signal(false);

        Self { visible, focus }
    }

    pub fn open(&self) {
        self.visible.set(true);
        self.focus.set(Focus::AboutPopup);
    }

    pub fn close(&self) {
        self.visible.set(false);
        self.focus.set(Focus::Workbench);
    }
}

impl KeyPressFocus for AboutData {
    /// Returns true for all conditions when visible, so that no keybindings
    /// leak through to the workbench behind this modal.
    fn check_condition(
        &self,
        _condition: crate::keypress::condition::Condition,
    ) -> bool {
        self.visible.get_untracked()
    }

    fn run_command(
        &self,
        command: &crate::command::LapceCommand,
        _count: Option<usize>,
        _mods: Modifiers,
    ) -> crate::command::CommandExecuted {
        match &command.kind {
            CommandKind::Workbench(_) => {}
            CommandKind::Edit(_) => {}
            CommandKind::Move(_) => {}
            CommandKind::Scroll(_) => {}
            CommandKind::Focus(cmd) => {
                if cmd == &FocusCommand::ModalClose {
                    self.close();
                }
            }
            CommandKind::MotionMode(_) => {}
            CommandKind::MultiSelection(_) => {}
        }
        CommandExecuted::Yes
    }

    fn receive_char(&self, _c: &str) {}

    /// Returning true prevents keyboard events from reaching the workbench behind this modal,
    /// ensuring the modal is truly exclusive while visible.
    fn focus_only(&self) -> bool {
        true
    }
}

pub fn about_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let about_data = workspace_data.about_data.clone();
    let config = workspace_data.common.config;
    let internal_command = workspace_data.common.internal_command;
    let logo_size = 100.0;

    let close_data = about_data.clone();
    exclusive_popup(
        config,
        about_data.visible,
        move || close_data.close(),
        move || {
            stack((
                svg(move || (config.get()).logo_svg()).style(move |s| {
                    s.size(logo_size, logo_size)
                        .color(config.get().color(LapceColor::EDITOR_FOREGROUND))
                }),
                label(|| "Lapce".to_string()).style(move |s| {
                    s.font_bold()
                        .margin_top(10.0)
                        .color(config.get().color(LapceColor::EDITOR_FOREGROUND))
                }),
                label(|| format!("Version: {}", VERSION)).style(move |s| {
                    s.margin_top(10.0)
                        .color(config.get().color(LapceColor::EDITOR_DIM))
                }),
                web_link(
                    || "Website".to_string(),
                    || AboutUri::LAPCE.to_string(),
                    move || config.get().color(LapceColor::EDITOR_LINK),
                    internal_command,
                )
                .style(|s| s.margin_top(20.0)),
                web_link(
                    || "GitHub".to_string(),
                    || AboutUri::GITHUB.to_string(),
                    move || config.get().color(LapceColor::EDITOR_LINK),
                    internal_command,
                )
                .style(|s| s.margin_top(10.0)),
                web_link(
                    || "Discord".to_string(),
                    || AboutUri::DISCORD.to_string(),
                    move || config.get().color(LapceColor::EDITOR_LINK),
                    internal_command,
                )
                .style(|s| s.margin_top(10.0)),
                web_link(
                    || "Matrix".to_string(),
                    || AboutUri::MATRIX.to_string(),
                    move || config.get().color(LapceColor::EDITOR_LINK),
                    internal_command,
                )
                .style(|s| s.margin_top(10.0)),
                label(|| "Attributions".to_string()).style(move |s| {
                    s.font_bold()
                        .color(config.get().color(LapceColor::EDITOR_DIM))
                        .margin_top(40.0)
                }),
                web_link(
                    || "Codicons (CC-BY-4.0)".to_string(),
                    || AboutUri::CODICONS.to_string(),
                    move || config.get().color(LapceColor::EDITOR_LINK),
                    internal_command,
                )
                .style(|s| s.margin_top(10.0)),
            ))
            .style(move |s| {
                let config = config.get();
                s.flex_col()
                    .items_center()
                    .padding_vert(25.0)
                    .padding_horiz(100.0)
                    .border(1.0)
                    .border_radius(LapceLayout::BORDER_RADIUS)
                    .border_color(config.color(LapceColor::LAPCE_BORDER))
                    .background(config.color(LapceColor::PANEL_BACKGROUND))
            })
        },
    )
    .debug_name("About Popup")
}

/// Reusable modal overlay pattern: a full-screen semi-transparent backdrop that
/// centers its content and closes when clicking outside. The inner content wrapper
/// stops PointerDown propagation so clicks inside the modal don't trigger the close.
/// The outer container also stops PointerMove to prevent hover effects on elements
/// behind the modal backdrop.
pub fn exclusive_popup<V: View + 'static>(
    config: ReadSignal<Arc<LapceConfig>>,
    visibility: RwSignal<bool>,
    on_close: impl Fn() + 'static,
    content: impl FnOnce() -> V,
) -> impl View {
    container(
        container(
            container(content())
                .on_event_stop(EventListener::PointerDown, move |_| {}),
        )
        .style(move |s| {
            s.flex_grow(1.0)
                .flex_row()
                .items_center()
                .hover(move |s| s.cursor(CursorStyle::Default))
        }),
    )
    .on_event_stop(EventListener::PointerDown, move |_| {
        on_close();
    })
    // Prevent things behind the grayed out area from being hovered.
    .on_event_stop(EventListener::PointerMove, move |_| {})
    .style(move |s| {
        s.display(if visibility.get() {
            Display::Flex
        } else {
            Display::None
        })
        .position(Position::Absolute)
        .size_pct(100.0, 100.0)
        .flex_col()
        .items_center()
        .background(
            config
                .get()
                .color(LapceColor::LAPCE_DROPDOWN_SHADOW)
                .multiply_alpha(LapceLayout::SHADOW_ALPHA),
        )
    })
}
