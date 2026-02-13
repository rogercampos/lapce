use std::{
    rc::Rc,
    sync::{Arc, atomic::AtomicU64},
};

use floem::{
    View,
    reactive::{
        Memo, ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith, create_memo,
    },
    style::{CursorStyle, Display},
    views::{Decorators, dyn_stack, label, stack, svg},
};
use indexmap::IndexMap;
use lsp_types::{DiagnosticSeverity, ProgressToken};

use crate::{
    app::clickable_icon,
    config::{LapceConfig, color::LapceColor, icon::LapceIcons},
    editor::EditorData,
    palette::kind::PaletteKind,
    panel::{kind::PanelKind, position::PanelContainerPosition},
    workspace_data::{WorkProgress, WorkspaceData},
};

pub fn status(
    workspace_data: Rc<WorkspaceData>,
    status_height: RwSignal<f64>,
    _config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let config = workspace_data.common.config;
    let diagnostics = workspace_data.main_split.diagnostics;
    let editor = workspace_data.main_split.active_editor;
    let panel = workspace_data.panel.clone();
    let palette = workspace_data.palette.clone();
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
    let progresses = workspace_data.progresses;

    stack((
        stack((
            {
                let panel = panel.clone();
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
                                .color(
                                    config
                                        .get()
                                        .color(LapceColor::STATUS_FOREGROUND),
                                )
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
                                .color(
                                    config
                                        .get()
                                        .color(LapceColor::STATUS_FOREGROUND),
                                )
                                .selectable(false)
                        },
                    ),
                ))
                .on_click_stop(move |_| {
                    panel.show_panel(&PanelKind::Problem);
                })
                .style(move |s| {
                    s.height_pct(100.0)
                        .padding_horiz(10.0)
                        .items_center()
                        .hover(|s| {
                            s.cursor(CursorStyle::Pointer).background(
                                config
                                    .get()
                                    .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                        })
                })
            },
            progress_view(config, progresses),
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
            let palette_clone = palette.clone();
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
                palette_clone.run(PaletteKind::Line);
            });
            let palette_clone = palette.clone();
            let line_ending_info = status_text(config, editor, move || {
                if let Some(editor) = editor.get() {
                    let doc = editor.doc_signal().get();
                    doc.buffer.with(|b| b.line_ending()).as_str()
                } else {
                    ""
                }
            })
            .on_click_stop(move |_| {
                palette_clone.run(PaletteKind::LineEnding);
            });
            let palette_clone = palette.clone();
            let language_info = status_text(config, editor, move || {
                if let Some(editor) = editor.get() {
                    let doc = editor.doc_signal().get();
                    doc.syntax().with(|s| s.language.name())
                } else {
                    "unknown"
                }
            })
            .on_click_stop(move |_| {
                palette_clone.run(PaletteKind::Language);
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
        s.border_top(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::STATUS_BACKGROUND))
            .flex_basis(config.ui.status_height() as f32)
            .flex_grow(0.0)
            .flex_shrink(0.0)
            .items_center()
    })
    .debug_name("Status/Bottom Bar")
}

fn progress_view(
    config: ReadSignal<Arc<LapceConfig>>,
    progresses: RwSignal<IndexMap<ProgressToken, WorkProgress>>,
) -> impl View {
    let id = AtomicU64::new(0);
    dyn_stack(
        move || progresses.get(),
        move |_| id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        move |(_, p)| {
            let progress = match p.message {
                Some(message) if !message.is_empty() => {
                    format!("{}: {}", p.title, message)
                }
                _ => p.title,
            };
            label(move || progress.clone()).style(move |s| {
                s.height_pct(100.0)
                    .min_width(0.0)
                    .margin_left(10.0)
                    .text_ellipsis()
                    .selectable(false)
                    .items_center()
                    .color(config.get().color(LapceColor::STATUS_FOREGROUND))
            })
        },
    )
    .style(move |s| s.flex_row().height_pct(100.0).min_width(0.0))
}

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
