use std::{
    ops::Range,
    rc::Rc,
    sync::{Arc, atomic::AtomicU64},
};

use floem::{
    View,
    event::EventListener,
    peniko::kurbo::{Point, Size},
    reactive::{ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith},
    style::{AlignItems, CursorStyle, Display, JustifyContent, Position},
    text::Weight,
    views::{
        Decorators, VirtualVector, container, dyn_stack,
        editor::{core::register::Clipboard, text::SystemClipboard},
        empty, rich_text,
        scroll::{PropagatePointerWheel, scroll},
        stack, svg, text, virtual_stack,
    },
};
use lsp_types::{CompletionItemKind, MessageType, ShowMessageParams};

use crate::{
    code_action::CodeActionStatus,
    config::{
        LapceConfig, color::LapceColor, icon::LapceIcons, layout::LapceLayout,
    },
    focus_text::focus_text,
    markdown::MarkdownContent,
    text_input::TextInputBuilder,
    workspace_data::WorkspaceData,
};

use super::clickable_icon;

pub(super) fn window_message_view(
    messages: RwSignal<Vec<(String, ShowMessageParams)>>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let view_fn =
        move |(i, (title, message)): (usize, (String, ShowMessageParams))| {
            stack((
                svg(move || {
                    if let MessageType::ERROR = message.typ {
                        config.get().ui_svg(LapceIcons::ERROR)
                    } else {
                        config.get().ui_svg(LapceIcons::WARNING)
                    }
                })
                .style(move |s| {
                    let config = config.get();
                    let size = config.ui.icon_size() as f32;
                    let color = if let MessageType::ERROR = message.typ {
                        config.color(LapceColor::LAPCE_ERROR)
                    } else {
                        config.color(LapceColor::LAPCE_WARN)
                    };
                    s.min_width(size)
                        .size(size, size)
                        .margin_right(10.0)
                        .margin_top(4.0)
                        .color(color)
                }),
                stack((
                    text(title.clone()).style(|s| {
                        s.min_width(0.0)
                            .line_height(LapceLayout::UI_LINE_HEIGHT as f32)
                            .font_weight(Weight::BOLD)
                    }),
                    text(message.message.clone()).style(|s| {
                        s.min_width(0.0)
                            .line_height(LapceLayout::UI_LINE_HEIGHT as f32)
                            .margin_top(5.0)
                    }),
                ))
                .style(move |s| {
                    s.flex_col().min_width(0.0).flex_basis(0.0).flex_grow(1.0)
                }),
                clickable_icon(
                    || LapceIcons::CLOSE,
                    move || {
                        messages.update(|messages| {
                            messages.remove(i);
                        });
                    },
                    || false,
                    || false,
                    || "Close",
                    config,
                )
                .style(|s| s.margin_left(6.0)),
            ))
            .on_double_click_stop(move |_| {
                messages.update(|messages| {
                    messages.remove(i);
                });
            })
            .on_secondary_click_stop({
                let message = message.message.clone();
                move |_| {
                    let mut clipboard = SystemClipboard::new();
                    if !message.is_empty() {
                        clipboard.put_string(&message);
                    }
                }
            })
            .on_event_stop(EventListener::PointerDown, |_| {})
            .style(move |s| {
                let config = config.get();
                s.width_full()
                    .items_start()
                    .padding(10.0)
                    .border(1.0)
                    .border_radius(LapceLayout::BORDER_RADIUS)
                    .border_color(config.color(LapceColor::LAPCE_BORDER))
                    .background(config.color(LapceColor::PANEL_BACKGROUND))
                    .apply_if(i > 0, |s| s.margin_top(10.0))
            })
        };

    let id = AtomicU64::new(0);
    container(
        container(
            container(
                scroll(
                    dyn_stack(
                        move || messages.get().into_iter().enumerate(),
                        move |_| {
                            id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        },
                        view_fn,
                    )
                    .style(|s| s.flex_col().width_full()),
                )
                .style(|s| {
                    s.absolute()
                        .pointer_events_auto()
                        .width_full()
                        .min_height(0.0)
                        .max_height_full()
                        .set(PropagatePointerWheel, false)
                }),
            )
            .style(|s| s.size_full()),
        )
        .style(|s| {
            s.width(360.0)
                .max_width_pct(LapceLayout::MODAL_MAX_PCT)
                .padding(10.0)
                .height_full()
        }),
    )
    .style(|s| s.absolute().size_full().justify_end().pointer_events_none())
    .debug_name("Window Message View")
}

pub(super) struct VectorItems<V>(pub(super) im::Vector<V>);

impl<V: Clone + 'static> VirtualVector<(usize, V)> for VectorItems<V> {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(&mut self, range: Range<usize>) -> impl Iterator<Item = (usize, V)> {
        let start = range.start;
        self.0
            .slice(range)
            .into_iter()
            .enumerate()
            .map(move |(i, item)| (i + start, item))
    }
}

pub(super) fn completion_kind_to_str(kind: CompletionItemKind) -> &'static str {
    match kind {
        CompletionItemKind::METHOD => "f",
        CompletionItemKind::FUNCTION => "f",
        CompletionItemKind::CLASS => "c",
        CompletionItemKind::STRUCT => "s",
        CompletionItemKind::VARIABLE => "v",
        CompletionItemKind::INTERFACE => "i",
        CompletionItemKind::ENUM => "e",
        CompletionItemKind::ENUM_MEMBER => "e",
        CompletionItemKind::FIELD => "v",
        CompletionItemKind::PROPERTY => "p",
        CompletionItemKind::CONSTANT => "d",
        CompletionItemKind::MODULE => "m",
        CompletionItemKind::KEYWORD => "k",
        CompletionItemKind::SNIPPET => "n",
        _ => "t",
    }
}

pub(super) fn hover(workspace_data: Rc<WorkspaceData>) -> impl View {
    let hover_data = workspace_data.common.hover.clone();
    let config = workspace_data.common.config;
    let id = AtomicU64::new(0);
    let layout_rect = workspace_data.common.hover.layout_rect;

    scroll(
        dyn_stack(
            move || hover_data.content.get(),
            move |_| id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            move |content| match content {
                MarkdownContent::Text(text_layout) => container(
                    rich_text(move || text_layout.clone())
                        .style(|s| s.max_width(600.0)),
                )
                .style(|s| s.max_width_full()),
                MarkdownContent::Image { .. } => container(empty()),
                MarkdownContent::Separator => container(empty().style(move |s| {
                    s.width_full()
                        .margin_vert(5.0)
                        .height(1.0)
                        .background(config.get().color(LapceColor::LAPCE_BORDER))
                })),
            },
        )
        .style(|s| s.flex_col().padding_horiz(10.0).padding_vert(5.0)),
    )
    .on_resize(move |rect| {
        layout_rect.set(rect);
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .on_event_stop(EventListener::PointerDown, |_| {})
    .style(move |s| {
        let active = workspace_data.common.hover.active.get();
        if !active {
            s.hide()
        } else {
            let config = config.get();
            if let Some(origin) = workspace_data.hover_origin() {
                s.absolute()
                    .margin_left(origin.x as f32)
                    .margin_top(origin.y as f32)
                    .max_height(300.0)
                    .border(1.0)
                    .border_radius(LapceLayout::BORDER_RADIUS)
                    .border_color(config.color(LapceColor::LAPCE_BORDER))
                    .background(config.color(LapceColor::PANEL_BACKGROUND))
                    .set(PropagatePointerWheel, false)
            } else {
                s.hide()
            }
        }
    })
    .debug_name("Hover Layer")
}

pub(super) fn completion(workspace_data: Rc<WorkspaceData>) -> impl View {
    let completion_data = workspace_data.common.completion;
    let active_editor = workspace_data.main_split.active_editor;
    let config = workspace_data.common.config;
    let active = completion_data.with_untracked(|c| c.active);
    let request_id = move || completion_data.with_untracked(|c| c.request_id);
    scroll(
        virtual_stack(
            move || completion_data.with(|c| VectorItems(c.filtered_items.clone())),
            move |(i, _item)| (request_id(), *i),
            move |(i, item)| {
                stack((
                    container(
                        text(
                            item.item.kind.map(completion_kind_to_str).unwrap_or(""),
                        )
                        .style(move |s| {
                            s.width_full()
                                .justify_content(Some(JustifyContent::Center))
                        }),
                    )
                    .style(move |s| {
                        let config = config.get();
                        let width = config.editor.line_height() as f32;
                        s.width(width)
                            .min_width(width)
                            .height_full()
                            .align_items(Some(AlignItems::Center))
                            .font_weight(Weight::BOLD)
                            .apply_opt(
                                config.completion_color(item.item.kind),
                                |s, c| s.color(c).background(c.multiply_alpha(0.3)),
                            )
                    }),
                    focus_text(
                        move || {
                            if config.get().editor.completion_item_show_detail {
                                item.item
                                    .detail
                                    .clone()
                                    .unwrap_or(item.item.label.clone())
                            } else {
                                item.item.label.clone()
                            }
                        },
                        move || item.indices.clone(),
                        move || config.get().color(LapceColor::EDITOR_FOCUS),
                    )
                    .on_click_stop(move |_| {
                        active.set(i);
                        if let Some(editor) = active_editor.get_untracked() {
                            editor.select_completion();
                        }
                    })
                    .on_event_stop(EventListener::PointerDown, |_| {})
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(5.0)
                            .min_width(0.0)
                            .align_items(Some(AlignItems::Center))
                            .size_full()
                            .cursor(CursorStyle::Pointer)
                            .apply_if(active.get() == i, |s| {
                                s.background(
                                    config.color(LapceColor::COMPLETION_CURRENT),
                                )
                            })
                            .hover(move |s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    }),
                ))
                .style(move |s| {
                    s.align_items(Some(AlignItems::Center))
                        .width_full()
                        .height(config.get().editor.line_height() as f32)
                })
            },
        )
        .item_size_fixed(move || config.get().editor.line_height() as f64)
        .style(|s| {
            s.align_items(Some(AlignItems::Center))
                .width_full()
                .flex_col()
        }),
    )
    .ensure_visible(move || {
        let config = config.get();
        let active = active.get();
        Size::new(1.0, config.editor.line_height() as f64)
            .to_rect()
            .with_origin(Point::new(
                0.0,
                active as f64 * config.editor.line_height() as f64,
            ))
    })
    .on_resize(move |rect| {
        completion_data.update(|c| {
            c.layout_rect = rect;
        });
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .style(move |s| {
        let config = config.get();
        let origin = workspace_data.completion_origin();
        s.position(Position::Absolute)
            .width(config.editor.completion_width as i32)
            .max_height(400.0)
            .margin_left(origin.x as f32)
            .margin_top(origin.y as f32)
            .background(config.color(LapceColor::COMPLETION_BACKGROUND))
            .font_family(config.editor.font_family.clone())
            .font_size(config.editor.font_size() as f32)
            .border_radius(LapceLayout::BORDER_RADIUS)
    })
    .debug_name("Completion Layer")
}

pub(super) fn code_action(workspace_data: Rc<WorkspaceData>) -> impl View {
    let config = workspace_data.common.config;
    let code_action = workspace_data.code_action;
    let (status, active) = code_action
        .with_untracked(|code_action| (code_action.status, code_action.active));
    scroll(
        container(
            dyn_stack(
                move || {
                    code_action.with(|code_action| {
                        code_action.filtered_items.clone().into_iter().enumerate()
                    })
                },
                move |(i, _item)| *i,
                move |(i, item)| {
                    container(
                        text(item.title().replace('\n', " "))
                            .style(|s| s.text_ellipsis().min_width(0.0)),
                    )
                    .on_click_stop(move |_| {
                        let code_action = code_action.get_untracked();
                        code_action.active.set(i);
                        code_action.select();
                    })
                    .on_event_stop(EventListener::PointerDown, |_| {})
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(10.0)
                            .align_items(Some(AlignItems::Center))
                            .min_width(0.0)
                            .width_full()
                            .line_height(LapceLayout::UI_LINE_HEIGHT as f32)
                            .border_radius(LapceLayout::BORDER_RADIUS)
                            .cursor(CursorStyle::Pointer)
                            .apply_if(active.get() == i, |s| {
                                s.background(
                                    config.color(LapceColor::COMPLETION_CURRENT),
                                )
                            })
                            .hover(move |s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    })
                },
            )
            .style(|s| s.width_full().flex_col()),
        )
        .style(|s| s.width_full().padding_vert(4.0)),
    )
    .ensure_visible(move || {
        let config = config.get();
        let active = active.get();
        Size::new(1.0, config.editor.line_height() as f64)
            .to_rect()
            .with_origin(Point::new(
                0.0,
                active as f64 * config.editor.line_height() as f64,
            ))
    })
    .on_resize(move |rect| {
        code_action.update(|c| {
            c.layout_rect = rect;
        });
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .style(move |s| {
        let origin = workspace_data.code_action_origin();
        s.display(match status.get() {
            CodeActionStatus::Inactive => Display::None,
            CodeActionStatus::Active => Display::Flex,
        })
        .position(Position::Absolute)
        .width(400.0)
        .max_height(400.0)
        .margin_left(origin.x as f32)
        .margin_top(origin.y as f32)
        .background(config.get().color(LapceColor::COMPLETION_BACKGROUND))
        .border_radius(LapceLayout::BORDER_RADIUS)
    })
    .debug_name("Code Action Layer")
}

pub(super) fn definition_picker(workspace_data: Rc<WorkspaceData>) -> impl View {
    let config = workspace_data.common.config;
    let definition_picker = workspace_data.definition_picker;
    let (status, active) =
        definition_picker.with_untracked(|picker| (picker.status, picker.active));
    scroll(
        container(
            dyn_stack(
                move || {
                    definition_picker
                        .with(|picker| picker.items.clone().into_iter().enumerate())
                },
                move |(i, _item)| *i,
                move |(i, item)| {
                    let path = item.display_path.clone();
                    let line = item.line_number;
                    let item_path = std::path::PathBuf::from(&item.display_path);
                    container(
                        stack((
                            crate::file_icon::file_icon_svg(config, move || {
                                item_path.clone()
                            }),
                            text(format!("{path}:{line}")),
                        ))
                        .style(|s| s.items_center()),
                    )
                    .on_click_stop(move |_| {
                        let picker = definition_picker.get_untracked();
                        picker.active.set(i);
                        picker.select();
                    })
                    .on_event_stop(EventListener::PointerDown, |_| {})
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(10.0)
                            .align_items(Some(AlignItems::Center))
                            .line_height(LapceLayout::UI_LINE_HEIGHT as f32)
                            .border_radius(LapceLayout::BORDER_RADIUS)
                            .cursor(CursorStyle::Pointer)
                            .apply_if(active.get() == i, |s| {
                                s.background(
                                    config.color(LapceColor::COMPLETION_CURRENT),
                                )
                            })
                            .hover(move |s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    })
                },
            )
            .style(|s| s.flex_col()),
        )
        .style(|s| s.padding_vert(4.0)),
    )
    .ensure_visible(move || {
        let config = config.get();
        let active = active.get();
        Size::new(1.0, config.editor.line_height() as f64)
            .to_rect()
            .with_origin(Point::new(
                0.0,
                active as f64 * config.editor.line_height() as f64,
            ))
    })
    .on_resize(move |rect| {
        definition_picker.update(|p| {
            p.layout_rect = rect;
        });
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .style(move |s| {
        let origin = workspace_data.definition_picker_origin();
        s.display(match status.get() {
            crate::definition_picker::DefinitionPickerStatus::Inactive => {
                Display::None
            }
            crate::definition_picker::DefinitionPickerStatus::Active => {
                Display::Flex
            }
        })
        .position(Position::Absolute)
        .min_width(400.0)
        .max_height(300.0)
        .margin_left(origin.x as f32)
        .margin_top(origin.y as f32)
        .background(config.get().color(LapceColor::COMPLETION_BACKGROUND))
        .border_radius(LapceLayout::BORDER_RADIUS)
    })
    .debug_name("Definition Picker Layer")
}

pub(super) fn rename(workspace_data: Rc<WorkspaceData>) -> impl View {
    let editor = workspace_data.rename.editor.clone();
    let active = workspace_data.rename.active;
    let layout_rect = workspace_data.rename.layout_rect;
    let config = workspace_data.common.config;

    container(
        container(
            TextInputBuilder::new()
                .is_focused(move || active.get())
                .build_editor(editor)
                .style(|s| s.width(150.0)),
        )
        .style(move |s| {
            let config = config.get();
            s.font_family(config.editor.font_family.clone())
                .font_size(config.editor.font_size() as f32)
                .border(1.0)
                .border_radius(LapceLayout::BORDER_RADIUS)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
    )
    .on_resize(move |rect| {
        layout_rect.set(rect);
    })
    .on_event_stop(EventListener::PointerMove, |_| {})
    .on_event_stop(EventListener::PointerDown, |_| {})
    .style(move |s| {
        let origin = workspace_data.rename_origin();
        s.position(Position::Absolute)
            .apply_if(!active.get(), |s| s.hide())
            .margin_left(origin.x as f32)
            .margin_top(origin.y as f32)
            .background(config.get().color(LapceColor::PANEL_BACKGROUND))
            .border_radius(LapceLayout::BORDER_RADIUS)
            .padding(6.0)
    })
    .debug_name("Rename Layer")
}
