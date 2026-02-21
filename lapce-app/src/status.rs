use std::{rc::Rc, sync::Arc, time::Duration};

use floem::{
    View,
    action::exec_after,
    event::EventListener,
    prelude::SignalTrack,
    reactive::{
        Memo, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        create_effect, create_memo,
    },
    style::{CursorStyle, Display, Position},
    views::{Decorators, container, dyn_stack, empty, label, scroll, stack, svg},
};
use indexmap::IndexMap;
use lsp_types::DiagnosticSeverity;

use crate::{
    app::clickable_icon,
    config::{
        LapceConfig, color::LapceColor, icon::LapceIcons, layout::LapceLayout,
    },
    editor::EditorData,
    panel::position::PanelContainerPosition,
    workspace_data::{BackgroundTaskInfo, BackgroundTaskState, WorkspaceData},
};

/// The status bar at the bottom of the workspace. Layout is a three-section row:
/// Left: diagnostic counts (errors/warnings) + background tasks indicator
/// Center-Right: panel toggle buttons (left, bottom, right panels)
/// Far-Right: cursor position (clickable, opens Go to Line), line ending, language
pub fn status(
    workspace_data: Rc<WorkspaceData>,
    status_height: RwSignal<f64>,
) -> impl View {
    let config = workspace_data.common.config;
    let diagnostics = workspace_data.main_split.diagnostics;
    let editor = workspace_data.main_split.active_editor;
    let panel = workspace_data.panel.clone();
    let diagnostic_count = create_memo(move |_| {
        let mut errors = 0;
        let mut warnings = 0;
        for (_, diagnostics) in diagnostics.get().iter() {
            for diagnostic in diagnostics.diagnostics.get().iter() {
                if let Some(severity) = diagnostic.severity {
                    match severity {
                        DiagnosticSeverity::ERROR => errors += 1,
                        DiagnosticSeverity::WARNING => warnings += 1,
                        _ => (),
                    }
                }
            }
        }
        (errors, warnings)
    });

    stack((
        stack((
            stack((
                svg(move || config.get().ui_svg(LapceIcons::ERROR)).style(
                    move |s| {
                        let config = config.get();
                        let size = config.ui.icon_size() as f32;
                        s.size(size, size)
                            .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    },
                ),
                label(move || diagnostic_count.get().0.to_string()).style(
                    move |s| {
                        s.margin_left(5.0)
                            .color(config.get().color(LapceColor::STATUS_FOREGROUND))
                            .selectable(false)
                    },
                ),
                svg(move || config.get().ui_svg(LapceIcons::WARNING)).style(
                    move |s| {
                        let config = config.get();
                        let size = config.ui.icon_size() as f32;
                        s.size(size, size)
                            .margin_left(5.0)
                            .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    },
                ),
                label(move || diagnostic_count.get().1.to_string()).style(
                    move |s| {
                        s.margin_left(5.0)
                            .color(config.get().color(LapceColor::STATUS_FOREGROUND))
                            .selectable(false)
                    },
                ),
            ))
            .style(move |s| s.height_pct(100.0).padding_horiz(10.0).items_center()),
            background_tasks_indicator(
                config,
                workspace_data.background_tasks,
                workspace_data.bg_tasks_popup_visible,
            ),
        ))
        .style(|s| {
            s.height_pct(100.0)
                .min_width(0.0)
                .flex_basis(0.0)
                .flex_grow(1.0)
                .items_center()
        }),
        stack((
            {
                let panel = panel.clone();
                let icon = {
                    let panel = panel.clone();
                    move || {
                        if panel
                            .is_container_shown(&PanelContainerPosition::Left, true)
                        {
                            LapceIcons::SIDEBAR_LEFT
                        } else {
                            LapceIcons::SIDEBAR_LEFT_OFF
                        }
                    }
                };
                clickable_icon(
                    icon,
                    move || {
                        panel.toggle_container_visual(&PanelContainerPosition::Left)
                    },
                    || false,
                    || false,
                    || "Toggle Left Panel",
                    config,
                )
            },
            {
                let panel = panel.clone();
                let icon = {
                    let panel = panel.clone();
                    move || {
                        if panel.is_container_shown(
                            &PanelContainerPosition::Bottom,
                            true,
                        ) {
                            LapceIcons::LAYOUT_PANEL
                        } else {
                            LapceIcons::LAYOUT_PANEL_OFF
                        }
                    }
                };
                clickable_icon(
                    icon,
                    move || {
                        panel
                            .toggle_container_visual(&PanelContainerPosition::Bottom)
                    },
                    || false,
                    || false,
                    || "Toggle Bottom Panel",
                    config,
                )
            },
            {
                let panel = panel.clone();
                let icon = {
                    let panel = panel.clone();
                    move || {
                        if panel
                            .is_container_shown(&PanelContainerPosition::Right, true)
                        {
                            LapceIcons::SIDEBAR_RIGHT
                        } else {
                            LapceIcons::SIDEBAR_RIGHT_OFF
                        }
                    }
                };
                clickable_icon(
                    icon,
                    move || {
                        panel.toggle_container_visual(&PanelContainerPosition::Right)
                    },
                    || false,
                    || false,
                    || "Toggle Right Panel",
                    config,
                )
            },
        ))
        .style(move |s| {
            s.height_pct(100.0)
                .items_center()
                .color(config.get().color(LapceColor::STATUS_FOREGROUND))
        }),
        stack({
            let go_to_line_data = workspace_data.go_to_line_data.clone();
            let cursor_info = status_text(config, editor, move || {
                if let Some(editor) = editor.get() {
                    let mut status = String::new();
                    let cursor = editor.cursor().get();
                    if let Some((line, column, character)) = editor
                        .doc_signal()
                        .get()
                        .buffer
                        .with(|buffer| cursor.get_line_col_char(buffer))
                    {
                        status = format!(
                            "Ln {}, Col {}, Char {}",
                            line + 1,
                            column + 1,
                            character,
                        );
                    }
                    if let Some(selection) = cursor.get_selection() {
                        let selection_range = selection.0.abs_diff(selection.1);

                        if selection.0 != selection.1 {
                            status =
                                format!("{status} ({selection_range} selected)");
                        }
                    }
                    let selection_count = cursor.get_selection_count();
                    if selection_count > 1 {
                        status = format!("{status} {selection_count} selections");
                    }
                    return status;
                }
                String::new()
            })
            .on_click_stop(move |_| {
                go_to_line_data.open();
            });
            let has_file_editor = move || -> Display {
                if editor
                    .get()
                    .map(|editor| {
                        editor.doc_signal().get().content.with(|c| {
                            use crate::doc::DocContent;
                            matches!(
                                c,
                                DocContent::File { .. } | DocContent::Scratch { .. }
                            )
                        })
                    })
                    .unwrap_or(false)
                {
                    Display::Flex
                } else {
                    Display::None
                }
            };
            let line_ending_info = label(move || {
                if let Some(editor) = editor.get() {
                    let doc = editor.doc_signal().get();
                    doc.buffer.with(|b| b.line_ending()).as_str().to_string()
                } else {
                    String::new()
                }
            })
            .style(move |s| {
                let config = config.get();
                s.display(has_file_editor())
                    .height_full()
                    .padding_horiz(10.0)
                    .items_center()
                    .color(config.color(LapceColor::STATUS_FOREGROUND))
                    .selectable(false)
            });
            let language_info = label(move || {
                if let Some(editor) = editor.get() {
                    let doc = editor.doc_signal().get();
                    doc.syntax().with(|s| s.language.name().to_string())
                } else {
                    "unknown".to_string()
                }
            })
            .style(move |s| {
                let config = config.get();
                s.display(has_file_editor())
                    .height_full()
                    .padding_horiz(10.0)
                    .items_center()
                    .color(config.color(LapceColor::STATUS_FOREGROUND))
                    .selectable(false)
            });
            (cursor_info, line_ending_info, language_info)
        })
        .style(|s| {
            s.height_pct(100.0)
                .flex_basis(0.0)
                .flex_grow(1.0)
                .justify_end()
        }),
    ))
    .on_resize(move |rect| {
        let height = rect.height();
        if height != status_height.get_untracked() {
            status_height.set(height);
        }
    })
    .style(move |s| {
        let config = config.get();
        s.flex_basis(config.ui.status_height() as f32)
            .flex_grow(0.0)
            .flex_shrink(0.0)
            .items_center()
    })
    .debug_name("Status/Bottom Bar")
}

/// A reusable status bar text item that auto-hides when no file/scratch document is active.
/// Only shows for File and Scratch docs, not for settings/keymap/plugin views.
fn status_text<S: std::fmt::Display + 'static>(
    config: ReadSignal<Arc<LapceConfig>>,
    editor: Memo<Option<EditorData>>,
    text: impl Fn() -> S + 'static,
) -> impl View {
    label(text).style(move |s| {
        let config = config.get();
        let display = if editor
            .get()
            .map(|editor| {
                editor.doc_signal().get().content.with(|c| {
                    use crate::doc::DocContent;
                    matches!(c, DocContent::File { .. } | DocContent::Scratch { .. })
                })
            })
            .unwrap_or(false)
        {
            Display::Flex
        } else {
            Display::None
        };

        s.display(display)
            .height_full()
            .padding_horiz(10.0)
            .items_center()
            .color(config.color(LapceColor::STATUS_FOREGROUND))
            .hover(|s| {
                s.cursor(CursorStyle::Pointer)
                    .background(config.color(LapceColor::PANEL_HOVERED_BACKGROUND))
            })
            .selectable(false)
    })
}

/// Gear icon + "Working..." label in the status bar.
/// Hidden when no background tasks are running.
/// Uses a timer-driven opacity pulse for animation.
fn background_tasks_indicator(
    config: ReadSignal<Arc<LapceConfig>>,
    background_tasks: RwSignal<IndexMap<u64, BackgroundTaskInfo>>,
    popup_visible: RwSignal<bool>,
) -> impl View {
    let cx = Scope::current();
    let has_tasks = create_memo(move |_| !background_tasks.with(|t| t.is_empty()));

    // Timer-driven pulse: a tick counter incremented every 50ms while tasks exist.
    let pulse_tick = cx.create_rw_signal(0u32);

    // When has_tasks becomes true, kick-start the pulse loop.
    create_effect(move |_| {
        if has_tasks.get() {
            pulse_tick.update(|t| *t = t.wrapping_add(1));
        }
    });

    // Self-scheduling timer loop: each tick schedules the next one.
    // Lifecycle: the loop self-terminates when has_tasks becomes false, because
    // the check `has_tasks.get_untracked()` prevents scheduling the next tick.
    // A new loop is started by the effect above when has_tasks transitions to true.
    create_effect(move |_| {
        pulse_tick.track();
        if has_tasks.get_untracked() {
            exec_after(Duration::from_millis(50), move |_| {
                pulse_tick.update(|t| *t = t.wrapping_add(1));
            });
        }
    });

    stack((
        svg(move || config.get().ui_svg(LapceIcons::BACKGROUND_WORKING)).style(
            move |s| {
                let config = config.get();
                let size = config.ui.icon_size() as f32;
                // Sinusoidal opacity pulse: 40 ticks = full cycle (2 seconds),
                // opacity oscillates between 0.4 and 1.0
                let phase =
                    (pulse_tick.get() % 40) as f64 / 40.0 * std::f64::consts::TAU;
                let alpha = 0.7 + 0.3 * phase.sin();
                s.size(size, size).color(
                    config
                        .color(LapceColor::LAPCE_ICON_ACTIVE)
                        .multiply_alpha(alpha as f32),
                )
            },
        ),
        label(move || {
            background_tasks.with(|tasks| {
                tasks
                    .values()
                    .rev()
                    .find(|t| t.state == BackgroundTaskState::Active)
                    .or_else(|| tasks.values().last())
                    .map(|t| format_task_display(t))
                    .unwrap_or_default()
            })
        })
        .style(move |s| {
            s.margin_left(4.0)
                .color(config.get().color(LapceColor::STATUS_FOREGROUND))
                .selectable(false)
        }),
    ))
    .on_click_stop(move |_| {
        popup_visible.update(|v| *v = !*v);
    })
    .style(move |s| {
        let config = config.get();
        let display = if has_tasks.get() {
            Display::Flex
        } else {
            Display::None
        };
        let fg = config.color(LapceColor::STATUS_FOREGROUND);
        s.display(display)
            .height_pct(100.0)
            .padding_horiz(6.0)
            .items_center()
            .border_radius(LapceLayout::BORDER_RADIUS)
            .cursor(CursorStyle::Pointer)
            .hover(move |s| s.background(fg.multiply_alpha(0.1)))
            .active(move |s| s.background(fg.multiply_alpha(0.2)))
    })
}

/// Floating popup listing all background operations (active + queued).
/// Positioned above the status bar, anchored to the left.
pub fn background_tasks_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let cx = Scope::current();
    let config = workspace_data.common.config;
    let background_tasks = workspace_data.background_tasks;
    let popup_visible = workspace_data.bg_tasks_popup_visible;
    let status_height = workspace_data.status_height;

    let has_tasks = create_memo(move |_| !background_tasks.with(|t| t.is_empty()));
    let visible = create_memo(move |_| popup_visible.get() && has_tasks.get());

    // Shared pulse tick for active task icons in the popup
    let popup_pulse_tick = cx.create_rw_signal(0u32);
    create_effect(move |_| {
        if visible.get() {
            popup_pulse_tick.update(|t| *t = t.wrapping_add(1));
        }
    });
    create_effect(move |_| {
        popup_pulse_tick.track();
        if visible.get_untracked() {
            exec_after(Duration::from_millis(50), move |_| {
                popup_pulse_tick.update(|t| *t = t.wrapping_add(1));
            });
        }
    });

    // Transparent overlay to catch outside clicks
    let overlay = empty()
        .on_event_stop(EventListener::PointerDown, move |_| {
            popup_visible.set(false);
        })
        .style(move |s| {
            s.position(Position::Absolute)
                .inset(0.0)
                .apply_if(!visible.get(), |s| s.hide())
        });

    // Popup content
    let popup = container(
        scroll(
            dyn_stack(
                move || background_tasks.get().into_iter().collect::<Vec<_>>(),
                move |(id, _)| *id,
                move |(task_id, _)| {
                    let task_id = task_id;
                    stack((
                        svg(move || {
                            let config = config.get();
                            let is_queued = background_tasks.with(|tasks| {
                                tasks.get(&task_id).is_some_and(|info| {
                                    info.state == BackgroundTaskState::Queued
                                })
                            });
                            if is_queued {
                                config.ui_svg(LapceIcons::BACKGROUND_QUEUED)
                            } else {
                                config.ui_svg(LapceIcons::BACKGROUND_WORKING)
                            }
                        })
                        .style(move |s| {
                            let config = config.get();
                            let size = config.ui.icon_size() as f32;
                            let is_queued = background_tasks.with(|tasks| {
                                tasks.get(&task_id).is_some_and(|info| {
                                    info.state == BackgroundTaskState::Queued
                                })
                            });
                            let color = if is_queued {
                                config.color(LapceColor::EDITOR_DIM)
                            } else {
                                let phase = (popup_pulse_tick.get() % 40) as f64
                                    / 40.0
                                    * std::f64::consts::TAU;
                                let alpha = 0.7 + 0.3 * phase.sin();
                                config
                                    .color(LapceColor::LAPCE_ICON_ACTIVE)
                                    .multiply_alpha(alpha as f32)
                            };
                            s.size(size, size).min_width(size).color(color)
                        }),
                        label(move || {
                            background_tasks.with(|tasks| {
                                tasks
                                    .get(&task_id)
                                    .map(format_task_display)
                                    .unwrap_or_default()
                            })
                        })
                        .style(move |s| {
                            let config = config.get();
                            let is_queued = background_tasks.with(|tasks| {
                                tasks.get(&task_id).is_some_and(|info| {
                                    info.state == BackgroundTaskState::Queued
                                })
                            });
                            let color = if is_queued {
                                config.color(LapceColor::EDITOR_DIM)
                            } else {
                                config.color(LapceColor::EDITOR_FOREGROUND)
                            };
                            s.margin_left(8.0)
                                .text_ellipsis()
                                .min_width(0.0)
                                .flex_grow(1.0)
                                .flex_shrink(1.0)
                                .color(color)
                                .selectable(false)
                        }),
                    ))
                    .style(move |s| {
                        s.width_full()
                            .items_center()
                            .padding_vert(4.0)
                            .padding_horiz(10.0)
                    })
                },
            )
            .style(|s| s.flex_col().width_full()),
        )
        .style(|s| s.width_full().max_height(300.0)),
    )
    .on_event_stop(EventListener::PointerDown, |_| {
        // Prevent clicks inside popup from reaching the overlay
    })
    .style(move |s| {
        let config = config.get();
        s.position(Position::Absolute)
            .inset_bottom(status_height.get() as f32)
            .inset_left(0.0)
            .width(400.0)
            .padding_vert(6.0)
            .background(config.color(LapceColor::PANEL_BACKGROUND))
            .border(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .border_radius(6.0)
            .apply_if(!visible.get(), |s| s.hide())
    });

    stack((overlay, popup)).style(move |s| {
        s.display(if visible.get() {
            Display::Flex
        } else {
            Display::None
        })
        .size_full()
        .position(Position::Absolute)
    })
}

/// Format the display text for a task, including message and percentage if available.
fn format_task_display(info: &BackgroundTaskInfo) -> String {
    let mut text = info.name.clone();
    if let Some(msg) = &info.message {
        if !msg.is_empty() {
            text = format!("{}: {}", text, msg);
        }
    }
    if let Some(pct) = info.percentage {
        text = format!("{} \u{2014} {}%", text, pct);
    }
    text
}
