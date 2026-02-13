use std::{ops::Range, path::PathBuf, rc::Rc, sync::Arc};

use floem::{
    View,
    event::EventListener,
    keyboard::Modifiers,
    peniko::kurbo::{Point, Size},
    reactive::{
        Memo, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        create_rw_signal,
    },
    style::{CursorStyle, Display},
    views::{
        Decorators, VirtualVector, container, label, scroll,
        scroll::PropagatePointerWheel, stack, text, virtual_stack,
    },
};
use lapce_core::{
    buffer::Buffer, command::FocusCommand, mode::Mode, selection::Selection,
};
use lapce_rpc::proxy::SearchMatch;
use lapce_xi_rope::Rope;

use crate::{
    about::exclusive_popup,
    command::{
        CommandExecuted, CommandKind, InternalCommand, LapceCommand,
        LapceWorkbenchCommand,
    },
    config::{LapceConfig, color::LapceColor},
    editor::view::editor_container_view,
    editor::{
        EditorData, EditorViewKind,
        location::{EditorLocation, EditorPosition},
    },
    focus_text::focus_text,
    global_search::GlobalSearchData,
    keypress::KeyPressFocus,
    main_split::MainSplitData,
    text_input::TextInputBuilder,
    workspace_data::{CommonData, Focus, WorkspaceData},
};

#[derive(Clone, Debug, PartialEq)]
pub struct FlatSearchMatch {
    pub path: PathBuf,
    pub search_match: SearchMatch,
}

#[derive(Clone)]
pub struct SearchModalData {
    pub visible: RwSignal<bool>,
    pub index: RwSignal<usize>,
    pub input_editor: EditorData,
    pub preview_editor: EditorData,
    pub has_preview: RwSignal<bool>,
    pub flat_matches: Memo<Vec<FlatSearchMatch>>,
    pub global_search: GlobalSearchData,
    pub main_split: MainSplitData,
    pub common: Rc<CommonData>,
    pub preview_focused: RwSignal<bool>,
}

impl std::fmt::Debug for SearchModalData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchModalData").finish()
    }
}

impl SearchModalData {
    pub fn new(
        cx: Scope,
        main_split: MainSplitData,
        global_search: GlobalSearchData,
        common: Rc<CommonData>,
    ) -> Self {
        let visible = cx.create_rw_signal(false);
        let index = cx.create_rw_signal(0usize);
        let input_editor = main_split.editors.make_local(cx, common.clone());
        let preview_editor = main_split.editors.make_local(cx, common.clone());
        preview_editor.kind.set(EditorViewKind::Preview);
        let has_preview = cx.create_rw_signal(false);

        // Sync input_editor text -> global_search pattern
        {
            let global_search = global_search.clone();
            let buffer = input_editor.doc().buffer;
            cx.create_effect(move |_| {
                let content = buffer.with(|b| b.to_string());
                global_search.set_pattern(content);
            });
        }

        // Create flat_matches memo from grouped search results
        let search_result = global_search.search_result;
        let flat_matches = cx.create_memo(move |_| {
            search_result.with(|results| {
                results
                    .iter()
                    .flat_map(|(path, match_data)| {
                        match_data.matches.get().into_iter().map({
                            let path = path.clone();
                            move |m| FlatSearchMatch {
                                path: path.clone(),
                                search_match: m,
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            })
        });

        // Reset index and auto-preview first match when results change
        {
            let preview_editor = preview_editor.clone();
            let main_split = main_split.clone();
            cx.create_effect(move |_| {
                let matches = flat_matches.get();
                index.set(0);
                if let Some(m) = matches.first() {
                    let (doc, new_doc) = main_split.get_doc(m.path.clone(), None);
                    preview_editor.update_doc(doc);
                    preview_editor.go_to_location(
                        EditorLocation {
                            path: m.path.clone(),
                            position: Some(EditorPosition::Line(
                                m.search_match.line.saturating_sub(1),
                            )),
                            scroll_offset: None,
                            same_editor_tab: false,
                        },
                        new_doc,
                        None,
                    );
                    has_preview.set(true);
                } else {
                    has_preview.set(false);
                }
            });
        }

        // Auto-close when focus changes away
        {
            let focus = common.focus;
            cx.create_effect(move |_| {
                let f = focus.get();
                if f != Focus::SearchModal && visible.get_untracked() {
                    visible.set(false);
                }
            });
        }

        let preview_focused = cx.create_rw_signal(false);

        Self {
            visible,
            index,
            input_editor,
            preview_editor,
            has_preview,
            flat_matches,
            global_search,
            main_split,
            common,
            preview_focused,
        }
    }

    pub fn open(&self) {
        self.input_editor.doc().reload(Rope::from(""), true);
        self.input_editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::caret(0)));
        self.index.set(0);
        self.has_preview.set(false);
        self.preview_focused.set(false);

        // Grab word at cursor from active editor
        if self.common.focus.get_untracked() == Focus::Workbench {
            let active_editor = self.main_split.active_editor.get_untracked();
            if let Some(word) = active_editor.map(|editor| editor.word_at_cursor()) {
                if !word.is_empty() {
                    let word_len = word.len();
                    self.input_editor.doc().reload(Rope::from(&word), true);
                    self.input_editor.cursor().update(|cursor| {
                        cursor.set_insert(Selection::region(0, word_len))
                    });
                }
            }
        }

        self.visible.set(true);
        self.common.focus.set(Focus::SearchModal);
    }

    pub fn close(&self) {
        self.visible.set(false);
        if self.common.focus.get_untracked() == Focus::SearchModal {
            self.common.focus.set(Focus::Workbench);
        }
    }

    pub fn select(&self) {
        let matches = self.flat_matches.get_untracked();
        let idx = self.index.get_untracked();
        if let Some(m) = matches.get(idx) {
            self.common
                .internal_command
                .send(InternalCommand::JumpToLocation {
                    location: EditorLocation {
                        path: m.path.clone(),
                        position: Some(EditorPosition::Line(
                            m.search_match.line.saturating_sub(1),
                        )),
                        scroll_offset: None,
                        same_editor_tab: false,
                    },
                });
        }
        self.close();
    }

    fn next(&self) {
        self.preview_focused.set(false);
        let len = self.flat_matches.with_untracked(|items| items.len());
        if len == 0 {
            return;
        }
        let index = self.index.get_untracked();
        if index + 1 < len {
            self.index.set(index + 1);
            self.preview_match(index + 1);
        }
    }

    fn previous(&self) {
        self.preview_focused.set(false);
        let index = self.index.get_untracked();
        if index > 0 {
            self.index.set(index - 1);
            self.preview_match(index - 1);
        }
    }

    pub fn preview_match(&self, idx: usize) {
        let matches = self.flat_matches.get_untracked();
        if let Some(m) = matches.get(idx) {
            let (doc, new_doc) = self.main_split.get_doc(m.path.clone(), None);
            self.preview_editor.update_doc(doc);
            self.preview_editor.go_to_location(
                EditorLocation {
                    path: m.path.clone(),
                    position: Some(EditorPosition::Line(
                        m.search_match.line.saturating_sub(1),
                    )),
                    scroll_offset: None,
                    same_editor_tab: false,
                },
                new_doc,
                None,
            );
            self.has_preview.set(true);
        }
    }

    pub fn open_full_results(&self) {
        self.close();
        self.common
            .internal_command
            .send(InternalCommand::OpenSearchPanel);
    }
}

impl KeyPressFocus for SearchModalData {
    fn get_mode(&self) -> Mode {
        Mode::Insert
    }

    fn check_condition(
        &self,
        condition: crate::keypress::condition::Condition,
    ) -> bool {
        use crate::keypress::condition::Condition;
        if self.preview_focused.get_untracked() {
            matches!(condition, Condition::ModalFocus | Condition::EditorFocus)
        } else {
            matches!(condition, Condition::ListFocus | Condition::ModalFocus)
        }
    }

    fn run_command(
        &self,
        command: &LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Focus(cmd) => match cmd {
                FocusCommand::ModalClose => {
                    if self.preview_focused.get_untracked() {
                        self.preview_focused.set(false);
                    } else {
                        self.close();
                    }
                }
                FocusCommand::ListNext => self.next(),
                FocusCommand::ListPrevious => self.previous(),
                FocusCommand::ListSelect => self.select(),
                _ => {
                    if self.preview_focused.get_untracked() {
                        return self
                            .preview_editor
                            .run_command(command, count, mods);
                    }
                    return CommandExecuted::No;
                }
            },
            CommandKind::Workbench(cmd) => match cmd {
                LapceWorkbenchCommand::SearchModalOpenFullResults => {
                    self.open_full_results();
                }
                _ => return CommandExecuted::No,
            },
            _ => {
                if self.preview_focused.get_untracked() {
                    return self.preview_editor.run_command(command, count, mods);
                }
                self.input_editor.run_command(command, count, mods);
            }
        }
        CommandExecuted::Yes
    }

    fn receive_char(&self, c: &str) {
        if self.preview_focused.get_untracked() {
            self.preview_editor.receive_char(c);
        } else {
            self.input_editor.receive_char(c);
        }
    }

    fn focus_only(&self) -> bool {
        true
    }
}

// -- View --

struct FlatSearchItems(Vec<FlatSearchMatch>);

impl VirtualVector<(usize, FlatSearchMatch)> for FlatSearchItems {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, FlatSearchMatch)> {
        let start = range.start;
        self.0[range]
            .iter()
            .cloned()
            .enumerate()
            .map(move |(i, item)| (i + start, item))
    }
}

pub fn search_modal_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.search_modal_data.clone();
    let config = workspace_data.common.config;
    let visibility = data.visible;
    let close_data = data.clone();

    exclusive_popup(
        config,
        visibility,
        move || close_data.close(),
        move || search_modal_content(workspace_data),
    )
    .debug_name("Search Modal Popup")
}

fn search_modal_content(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.search_modal_data.clone();
    let config = workspace_data.common.config;
    let focus = workspace_data.common.focus;
    let index = data.index;
    let flat_matches = data.flat_matches;
    let has_preview = data.has_preview;
    let item_height = 26.0;
    let input_buffer = data.input_editor.doc().buffer;

    stack((
        // Header: Search input
        search_modal_input(data.clone(), config, focus),
        // Body: results list + preview (fixed size via flex_grow)
        search_modal_body(
            workspace_data.clone(),
            data.clone(),
            config,
            index,
            flat_matches,
            has_preview,
            input_buffer,
            item_height,
        ),
        // Footer: "Open full results" button
        search_modal_footer(data, config),
    ))
    .style(move |s| {
        let config = config.get();
        s.flex_col()
            .width(800.0)
            .height(600.0)
            .max_width_pct(80.0)
            .max_height_pct(80.0)
            .border(1.0)
            .border_radius(6.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::PALETTE_BACKGROUND))
    })
}

fn search_modal_input(
    data: SearchModalData,
    config: ReadSignal<Arc<LapceConfig>>,
    focus: RwSignal<Focus>,
) -> impl View {
    let preview_focused = data.preview_focused;
    let is_focused =
        move || focus.get() == Focus::SearchModal && !preview_focused.get();
    let input = TextInputBuilder::new()
        .is_focused(is_focused)
        .build_editor(data.input_editor.clone())
        .placeholder(|| "Search in files...".to_owned())
        .style(|s| s.width_full());

    container(
        container(input)
            .on_event_cont(EventListener::PointerDown, move |_| {
                preview_focused.set(false);
            })
            .style(move |s| {
                let config = config.get();
                s.width_full()
                    .height(30.0)
                    .items_center()
                    .border_bottom(1.0)
                    .border_color(config.color(LapceColor::LAPCE_BORDER))
                    .background(config.color(LapceColor::EDITOR_BACKGROUND))
            }),
    )
    .style(|s| s.padding_bottom(5.0))
}

fn search_modal_body(
    workspace_data: Rc<WorkspaceData>,
    data: SearchModalData,
    config: ReadSignal<Arc<LapceConfig>>,
    index: RwSignal<usize>,
    flat_matches: Memo<Vec<FlatSearchMatch>>,
    has_preview: RwSignal<bool>,
    input_buffer: RwSignal<Buffer>,
    item_height: f64,
) -> impl View {
    stack((
        // When there are matches: show results list + preview
        stack((
            // Results list
            scroll({
                let data = data.clone();
                virtual_stack(
                    move || FlatSearchItems(flat_matches.get()),
                    move |(i, m)| {
                        (
                            *i,
                            m.path.clone(),
                            m.search_match.line,
                            m.search_match.start,
                            m.search_match.end,
                        )
                    },
                    move |(i, m)| {
                        let data = data.clone();
                        let double_click_data = data.clone();
                        let line_content = m.search_match.line_content.clone();
                        let line_number = m.search_match.line;
                        let start = m.search_match.start;
                        let end = m.search_match.end;
                        let filename = m
                            .path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let location_label = format!("{}:{}", filename, line_number);
                        let line_content_for_trim = line_content.clone();

                        container(
                            stack((
                                focus_text(
                                    move || {
                                        let config = config.get();
                                        if config.ui.trim_search_results_whitespace {
                                            line_content.trim().to_string()
                                        } else {
                                            line_content.clone()
                                        }
                                    },
                                    move || {
                                        let config = config.get();
                                        let offset = if config
                                            .ui
                                            .trim_search_results_whitespace
                                        {
                                            line_content_for_trim.trim_start().len()
                                                as i32
                                                - line_content_for_trim.len() as i32
                                        } else {
                                            0
                                        };
                                        ((start as i32 + offset) as usize
                                            ..(end as i32 + offset) as usize)
                                            .collect()
                                    },
                                    move || {
                                        config.get().color(LapceColor::EDITOR_FOCUS)
                                    },
                                )
                                .style(|s| s.min_width(0.0)),
                                container(text(""))
                                    .style(|s| s.flex_grow(1.0).min_width(10.0)),
                                label(move || location_label.clone()).style(
                                    move |s| {
                                        s.color(
                                            config
                                                .get()
                                                .color(LapceColor::EDITOR_DIM),
                                        )
                                        .flex_shrink(0.0)
                                    },
                                ),
                            ))
                            .style(|s| s.width_full().items_center()),
                        )
                        .on_click_stop(move |_| {
                            data.preview_focused.set(false);
                            data.index.set(i);
                            data.preview_match(i);
                        })
                        .on_double_click_stop(move |_| {
                            double_click_data.index.set(i);
                            double_click_data.select();
                        })
                        .style(move |s| {
                            let is_selected = index.get() == i;
                            let config = config.get();
                            s.width_full()
                                .height(item_height as f32)
                                .padding_horiz(10.0)
                                .items_center()
                                .cursor(CursorStyle::Pointer)
                                .apply_if(is_selected, |s| {
                                    s.background(config.color(
                                        LapceColor::PALETTE_CURRENT_BACKGROUND,
                                    ))
                                })
                                .hover(|s| {
                                    s.background(
                                        config.color(
                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                        ),
                                    )
                                })
                        })
                    },
                )
                .item_size_fixed(move || item_height)
                .style(|s| s.width_full().flex_col())
            })
            .ensure_visible(move || {
                Size::new(1.0, item_height)
                    .to_rect()
                    .with_origin(Point::new(0.0, index.get() as f64 * item_height))
            })
            .style(move |s| {
                s.width_full()
                    .min_height(0.0)
                    .flex_basis(0.0)
                    .flex_grow(1.0)
                    .set(PropagatePointerWheel, false)
            }),
            // Preview editor (50% of body)
            search_modal_preview_editor(workspace_data, config),
        ))
        .style(move |s| {
            s.display(if has_preview.get() {
                Display::Flex
            } else {
                Display::None
            })
            .size_full()
            .flex_col()
        }),
        // When no matches: placeholder text
        container(
            label(move || {
                let input_text = input_buffer.with(|b| b.to_string());
                let is_empty = flat_matches.with(|items| items.is_empty());
                if input_text.is_empty() {
                    "Type search query to find in files".to_string()
                } else if is_empty {
                    "No results".to_string()
                } else {
                    String::new()
                }
            })
            .style(move |s| s.color(config.get().color(LapceColor::EDITOR_DIM))),
        )
        .style(move |s| {
            let config = config.get();
            s.display(if has_preview.get() {
                Display::None
            } else {
                Display::Flex
            })
            .size_full()
            .items_center()
            .justify_center()
            .border_top(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
    ))
    .style(|s| s.flex_grow(1.0).min_height(0.0))
}

fn search_modal_preview_editor(
    workspace_data: Rc<WorkspaceData>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let data = workspace_data.search_modal_data.clone();
    let preview_focused = data.preview_focused;
    let workspace = workspace_data.workspace.clone();
    let preview_editor = create_rw_signal(data.preview_editor.clone());

    container(
        container(editor_container_view(
            workspace_data,
            workspace,
            |_tracked: bool| true,
            preview_editor,
        ))
        .on_event_cont(EventListener::PointerDown, move |_| {
            preview_focused.set(true);
        })
        .style(move |s| {
            let config = config.get();
            s.position(floem::style::Position::Absolute)
                .border_top(1.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .size_full()
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
    )
    .style(|s| s.flex_basis(0.0).flex_grow(1.0).min_height(0.0))
}

fn search_modal_footer(
    data: SearchModalData,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    stack((
        label(|| "Open in search panel".to_string()).style(move |s| {
            s.color(config.get().color(LapceColor::EDITOR_DIM))
                .font_size(12.0)
        }),
        container(text("")).style(|s| s.flex_grow(1.0)),
        label(|| {
            let modifier = if cfg!(target_os = "macos") {
                "\u{2318}"
            } else {
                "Ctrl"
            };
            format!("{modifier}+Enter")
        })
        .style(move |s| {
            let config = config.get();
            s.color(config.color(LapceColor::EDITOR_DIM))
                .font_size(11.0)
                .padding_horiz(6.0)
                .padding_vert(2.0)
                .border(1.0)
                .border_radius(3.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
        }),
    ))
    .on_click_stop(move |_| {
        data.open_full_results();
    })
    .style(move |s| {
        let config = config.get();
        s.width_full()
            .padding_horiz(12.0)
            .padding_vert(6.0)
            .border_top(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .items_center()
            .cursor(CursorStyle::Pointer)
            .hover(|s| {
                s.background(config.color(LapceColor::PANEL_HOVERED_BACKGROUND))
            })
    })
}
