use std::rc::Rc;

use floem::{
    View,
    event::EventListener,
    keyboard::Modifiers,
    reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    style::CursorStyle,
    views::{Decorators, container, label, stack},
};
use lapce_core::{command::FocusCommand, mode::Mode, selection::Selection};
use lapce_xi_rope::Rope;

use crate::{
    about::exclusive_popup,
    command::{CommandExecuted, CommandKind, InternalCommand, LapceCommand},
    config::{color::LapceColor, layout::LapceLayout},
    editor::EditorData,
    editor::location::{EditorLocation, EditorPosition},
    keypress::KeyPressFocus,
    main_split::MainSplitData,
    text_input::TextInputBuilder,
    workspace_data::{CommonData, Focus, WorkspaceData},
};

#[derive(Clone)]
pub struct GoToLineData {
    pub visible: RwSignal<bool>,
    pub input_editor: EditorData,
    pub main_split: MainSplitData,
    pub common: Rc<CommonData>,
}

impl std::fmt::Debug for GoToLineData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoToLineData").finish()
    }
}

impl GoToLineData {
    pub fn new(
        cx: Scope,
        main_split: MainSplitData,
        common: Rc<CommonData>,
    ) -> Self {
        let visible = cx.create_rw_signal(false);
        let input_editor = main_split.editors.make_local(cx, common.clone());

        {
            let visible = visible;
            let focus = common.focus;
            cx.create_effect(move |_| {
                let f = focus.get();
                if f != Focus::GoToLine && visible.get_untracked() {
                    visible.set(false);
                }
            });
        }

        Self {
            visible,
            input_editor,
            main_split,
            common,
        }
    }

    pub fn open(&self) {
        self.input_editor.doc().reload(Rope::from(""), true);
        self.input_editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::caret(0)));
        self.visible.set(true);
        self.common.focus.set(Focus::GoToLine);
    }

    pub fn close(&self) {
        self.visible.set(false);
        if self.common.focus.get_untracked() == Focus::GoToLine {
            self.common.focus.set(Focus::Workbench);
        }
    }

    pub fn go_to_line(&self) {
        let input_text = self.input_editor.doc().buffer.get_untracked().to_string();
        let line: usize = match input_text.trim().parse() {
            Ok(n) if n > 0 => n,
            _ => {
                self.close();
                return;
            }
        };

        let editor = self.main_split.active_editor.get_untracked();
        let doc = match editor {
            Some(editor) => editor.doc(),
            None => {
                self.close();
                return;
            }
        };
        let path = doc
            .content
            .with_untracked(|content| content.path().cloned());
        let path = match path {
            Some(path) => path,
            None => {
                self.close();
                return;
            }
        };

        self.common
            .internal_command
            .send(InternalCommand::JumpToLocation {
                location: EditorLocation {
                    path,
                    position: Some(EditorPosition::Line(line - 1)),
                    scroll_offset: None,
                    same_editor_tab: false,
                },
            });
        self.close();
    }
}

impl KeyPressFocus for GoToLineData {
    fn get_mode(&self) -> Mode {
        Mode::Insert
    }

    fn check_condition(
        &self,
        condition: crate::keypress::condition::Condition,
    ) -> bool {
        matches!(
            condition,
            crate::keypress::condition::Condition::ModalFocus
                | crate::keypress::condition::Condition::ListFocus
        )
    }

    fn run_command(
        &self,
        command: &LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Focus(cmd) => match cmd {
                FocusCommand::ModalClose => self.close(),
                FocusCommand::ListSelect => self.go_to_line(),
                _ => return CommandExecuted::No,
            },
            _ => {
                self.input_editor.run_command(command, count, mods);
            }
        }
        CommandExecuted::Yes
    }

    fn receive_char(&self, c: &str) {
        self.input_editor.receive_char(c);
    }

    fn focus_only(&self) -> bool {
        true
    }
}

pub fn go_to_line_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.go_to_line_data.clone();
    let config = workspace_data.common.config;
    let visibility = data.visible;
    let close_data = data.clone();

    exclusive_popup(
        config,
        visibility,
        move || close_data.close(),
        move || go_to_line_content(workspace_data),
    )
    .debug_name("Go To Line Popup")
}

fn go_to_line_content(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.go_to_line_data.clone();
    let config = workspace_data.common.config;
    let focus = workspace_data.common.focus;

    let is_focused = move || focus.get() == Focus::GoToLine;
    let input = TextInputBuilder::new()
        .is_focused(is_focused)
        .build_editor(data.input_editor.clone())
        .placeholder(|| "Line number...".to_owned())
        .style(|s| s.width_full());

    let go_data = data.clone();

    stack((
        container(container(input).style(move |s| {
            let config = config.get();
            s.width_full()
                .height(30.0)
                .items_center()
                .border_bottom(1.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }))
        .style(|s| s.padding_bottom(5.0)),
        container(
            label(|| "Go to Line".to_string())
                .on_event_stop(EventListener::PointerDown, |_| {})
                .on_click_stop(move |_| {
                    go_data.go_to_line();
                })
                .style(move |s| {
                    let config = config.get();
                    s.padding_horiz(16.0)
                        .padding_vert(6.0)
                        .border_radius(4.0)
                        .cursor(CursorStyle::Pointer)
                        .color(
                            config
                                .color(LapceColor::LAPCE_BUTTON_PRIMARY_FOREGROUND),
                        )
                        .background(
                            config
                                .color(LapceColor::LAPCE_BUTTON_PRIMARY_BACKGROUND),
                        )
                        .hover(|s| {
                            s.background(
                                config
                                    .color(
                                        LapceColor::LAPCE_BUTTON_PRIMARY_BACKGROUND,
                                    )
                                    .multiply_alpha(0.8),
                            )
                        })
                }),
        )
        .style(|s| {
            s.width_full()
                .padding_horiz(10.0)
                .padding_bottom(10.0)
                .justify_end()
        }),
    ))
    .style(move |s| {
        let config = config.get();
        s.flex_col()
            .width(300.0)
            .max_width_pct(LapceLayout::MODAL_MAX_PCT)
            .border(1.0)
            .border_radius(LapceLayout::BORDER_RADIUS)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::PALETTE_BACKGROUND))
    })
}
