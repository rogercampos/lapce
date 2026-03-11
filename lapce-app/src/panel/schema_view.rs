use std::{path::PathBuf, rc::Rc};

use floem::{
    View,
    reactive::{Memo, SignalGet, SignalWith, create_memo},
    style::CursorStyle,
    views::{Decorators, dyn_stack, label, scroll, stack},
};
use lapce_rpc::schema::{SchemaTable, model_path_to_table_name};

use super::position::PanelPosition;
use crate::{
    command::InternalCommand,
    config::color::LapceColor,
    doc::DocContent,
    editor::location::{EditorLocation, EditorPosition},
    listener::Listener,
    workspace_data::WorkspaceData,
};

/// Creates the Schema panel view showing AR model field information.
pub fn schema_panel(
    workspace_data: Rc<WorkspaceData>,
    _position: PanelPosition,
) -> impl View {
    let config = workspace_data.common.config;
    let main_split = workspace_data.main_split.clone();
    let schema_infos = workspace_data.schema_infos;
    let internal_command = workspace_data.common.internal_command;

    // Track the active editor's schema table + project root
    let schema_data: Memo<Option<(SchemaTable, PathBuf)>> = create_memo(move |_| {
        let editor = main_split.active_editor.get()?;
        let doc = editor.doc();
        let content = doc.content.get();
        let file_path = match &content {
            DocContent::File { path, .. } => path.clone(),
            _ => return None,
        };

        schema_infos.with(|infos| {
            for schema in infos.values() {
                if file_path.starts_with(&schema.project_root) {
                    let relative =
                        file_path.strip_prefix(&schema.project_root).ok()?;
                    let table_name =
                        model_path_to_table_name(relative, &schema.tables)?;
                    let table = schema.tables.get(&table_name).cloned()?;
                    return Some((table, schema.project_root.clone()));
                }
            }
            None
        })
    });

    let schema_table: Memo<Option<SchemaTable>> =
        create_memo(move |_| schema_data.get().map(|(t, _)| t));

    let schema_root: Memo<Option<PathBuf>> =
        create_memo(move |_| schema_data.get().map(|(_, r)| r));

    scroll(
        stack((
            // "No schema" message
            label(move || {
                if schema_table.get().is_some() {
                    String::new()
                } else {
                    "No schema for this file".to_string()
                }
            })
            .style(move |s| {
                let config = config.get();
                s.color(config.color(LapceColor::EDITOR_DIM))
                    .font_size(config.editor.font_size() as f32 * 0.9)
                    .padding(10.0)
                    .selectable(false)
                    .apply_if(schema_table.get().is_some(), |s| s.hide())
            }),
            // Columns header
            label(move || {
                let count = schema_table.get().map(|t| t.columns.len()).unwrap_or(0);
                format!("COLUMNS ({count})")
            })
            .style(move |s| {
                let config = config.get();
                s.width_pct(100.0)
                    .padding_horiz(10.0)
                    .padding_vert(6.0)
                    .font_size(config.editor.font_size() as f32 * 0.8)
                    .font_family(config.editor.font_family.clone())
                    .color(config.color(LapceColor::EDITOR_DIM))
                    .selectable(false)
                    .apply_if(schema_table.get().is_none(), |s| s.hide())
            }),
            // Column rows
            dyn_stack(
                move || {
                    schema_table
                        .get()
                        .map(|t| t.columns)
                        .unwrap_or_default()
                        .into_iter()
                        .enumerate()
                },
                |(i, col)| (*i, col.name.clone()),
                move |(_, col)| {
                    let name = col.name.clone();
                    let col_type = col.col_type.clone();
                    let line = col.line;
                    let constraints = {
                        let mut parts = Vec::new();
                        if !col.null {
                            parts.push("NOT NULL".to_string());
                        }
                        if let Some(ref d) = col.default {
                            parts.push(format!("default: {d}"));
                        }
                        parts.join(", ")
                    };

                    stack((
                        label(move || name.clone()).style(move |s| {
                            let config = config.get();
                            s.font_size(config.editor.font_size() as f32 * 0.9)
                                .font_family(config.editor.font_family.clone())
                                .color(config.color(LapceColor::EDITOR_FOREGROUND))
                                .min_width(120.0)
                                .selectable(false)
                        }),
                        label(move || col_type.clone()).style(move |s| {
                            let config = config.get();
                            s.font_size(config.editor.font_size() as f32 * 0.9)
                                .font_family(config.editor.font_family.clone())
                                .color(config.color(LapceColor::EDITOR_DIM))
                                .min_width(80.0)
                                .selectable(false)
                        }),
                        label(move || constraints.clone()).style(move |s| {
                            let config = config.get();
                            s.font_size(config.editor.font_size() as f32 * 0.85)
                                .font_family(config.editor.font_family.clone())
                                .color(config.color(LapceColor::EDITOR_DIM))
                                .selectable(false)
                        }),
                    ))
                    .on_click_stop(move |_| {
                        jump_to_schema_line(&schema_root, line, &internal_command);
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(10.0)
                            .padding_vert(3.0)
                            .width_pct(100.0)
                            .items_center()
                            .gap(8.0)
                            .cursor(CursorStyle::Pointer)
                            .hover(|s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    })
                },
            )
            .style(|s| s.flex_col().width_pct(100.0)),
            // Indexes header
            label(move || {
                let count = schema_table.get().map(|t| t.indexes.len()).unwrap_or(0);
                format!("INDEXES ({count})")
            })
            .style(move |s| {
                let config = config.get();
                s.width_pct(100.0)
                    .padding_horiz(10.0)
                    .padding_vert(6.0)
                    .margin_top(8.0)
                    .font_size(config.editor.font_size() as f32 * 0.8)
                    .font_family(config.editor.font_family.clone())
                    .color(config.color(LapceColor::EDITOR_DIM))
                    .selectable(false)
                    .apply_if(schema_table.get().is_none(), |s| s.hide())
            }),
            // Index rows
            dyn_stack(
                move || {
                    schema_table
                        .get()
                        .map(|t| t.indexes)
                        .unwrap_or_default()
                        .into_iter()
                        .enumerate()
                },
                |(i, idx)| (*i, idx.columns.join(",")),
                move |(_, idx)| {
                    let columns_str = idx.columns.join(", ");
                    let unique = idx.unique;
                    let name = idx.name.clone();
                    let line = idx.line;

                    stack((
                        label(move || columns_str.clone()).style(move |s| {
                            let config = config.get();
                            s.font_size(config.editor.font_size() as f32 * 0.9)
                                .font_family(config.editor.font_family.clone())
                                .color(config.color(LapceColor::EDITOR_FOREGROUND))
                                .selectable(false)
                        }),
                        label(move || {
                            if unique {
                                "UNIQUE".to_string()
                            } else {
                                String::new()
                            }
                        })
                        .style(move |s| {
                            let config = config.get();
                            s.font_size(config.editor.font_size() as f32 * 0.8)
                                .font_family(config.editor.font_family.clone())
                                .color(config.color(LapceColor::EDITOR_DIM))
                                .selectable(false)
                        }),
                        label(move || {
                            name.as_ref()
                                .map(|n| format!("({n})"))
                                .unwrap_or_default()
                        })
                        .style(move |s| {
                            let config = config.get();
                            s.font_size(config.editor.font_size() as f32 * 0.8)
                                .font_family(config.editor.font_family.clone())
                                .color(config.color(LapceColor::EDITOR_DIM))
                                .selectable(false)
                        }),
                    ))
                    .on_click_stop(move |_| {
                        jump_to_schema_line(&schema_root, line, &internal_command);
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(10.0)
                            .padding_vert(3.0)
                            .width_pct(100.0)
                            .items_center()
                            .gap(8.0)
                            .cursor(CursorStyle::Pointer)
                            .hover(|s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    })
                },
            )
            .style(|s| s.flex_col().width_pct(100.0)),
        ))
        .style(|s| s.flex_col().width_pct(100.0)),
    )
    .style(|s| s.absolute().size_pct(100.0, 100.0))
}

fn jump_to_schema_line(
    schema_root: &Memo<Option<PathBuf>>,
    line: usize,
    internal_command: &Listener<InternalCommand>,
) {
    if let Some(root) = schema_root.get_untracked() {
        let schema_path = root.join("db/schema.rb");
        internal_command.send(InternalCommand::JumpToLocation {
            location: EditorLocation {
                path: schema_path,
                position: Some(EditorPosition::Line(line.saturating_sub(1))),
                scroll_offset: None,
                same_editor_tab: false,
            },
        });
    }
}
