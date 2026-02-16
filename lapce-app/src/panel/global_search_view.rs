use std::{path::PathBuf, rc::Rc, sync::Arc};

use floem::{
    View,
    event::EventListener,
    reactive::{ReadSignal, RwSignal, SignalGet, SignalUpdate, create_rw_signal},
    style::{CursorStyle, Display},
    views::{
        Decorators, container, label, resizable::resizable, scroll, stack, svg,
        virtual_stack,
    },
};

use super::position::PanelPosition;
use crate::{
    command::InternalCommand,
    config::{
        LapceConfig, color::LapceColor, icon::LapceIcons, layout::LapceLayout,
    },
    editor::location::{EditorLocation, EditorPosition},
    editor::view::editor_container_view,
    file_icon::file_icon_svg,
    focus_text::focus_text_with_syntax,
    global_search::{GlobalSearchData, SearchTreeRow, SearchTreeVirtualList},
    listener::Listener,
    main_split::MainSplitData,
    workspace_data::WorkspaceData,
};

/// The search panel shows a 50/50 horizontal split: hierarchical results on the left,
/// preview editor on the right. Unlike the search modal (flat list + centered popup),
/// this is a persistent bottom panel that groups results by file with collapsible
/// file headers.
pub fn global_search_panel(
    workspace_data: Rc<WorkspaceData>,
    _position: PanelPosition,
) -> impl View {
    let global_search = workspace_data.global_search.clone();
    let config = global_search.common.config;
    let has_preview = global_search.has_preview;
    let preview_focused = global_search.preview_focused;

    resizable((
        search_result(global_search, config).style(move |s| {
            s.height_pct(100.0)
                .min_width(0)
                .flex_basis(0)
                .flex_grow(1.0)
        }),
        search_preview_editor(workspace_data, config, has_preview, preview_focused),
    ))
    .style(|s| s.absolute().size_pct(100.0, 100.0).flex_row())
    .debug_name("Global Search Panel")
}

/// Renders the folder-tree search results using a single flat virtual_stack.
/// Each row is a SearchTreeRow: Folder, File, or Match.
fn search_result(
    global_search_data: GlobalSearchData,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let ui_line_height = global_search_data.common.ui_line_height;
    let search_tree_rows = global_search_data.search_tree_rows;
    let selected_index = global_search_data.selected_index;
    let selected_match = global_search_data.selected_match;
    let internal_command = global_search_data.common.internal_command;
    let main_split = global_search_data.main_split.clone();

    container({
        scroll({
            virtual_stack(
                move || SearchTreeVirtualList(search_tree_rows.get()),
                move |row| row.key(),
                move |row| {
                    let gs = global_search_data.clone();
                    let ms = main_split.clone();
                    search_tree_row_view(
                        row,
                        gs,
                        config,
                        ui_line_height,
                        selected_index,
                        selected_match,
                        internal_command,
                        ms,
                    )
                },
            )
            .item_size_fixed(move || ui_line_height.get())
            .style(|s| {
                s.flex_col()
                    .min_width_pct(100.0)
                    .line_height(LapceLayout::UI_LINE_HEIGHT as f32)
            })
        })
        .style(|s| s.absolute().size_pct(100.0, 100.0))
    })
    .style(|s| s.size_pct(100.0, 100.0))
}

/// Render a single row of the search tree.
fn search_tree_row_view(
    row: SearchTreeRow,
    global_search: GlobalSearchData,
    config: ReadSignal<Arc<LapceConfig>>,
    ui_line_height: floem::reactive::Memo<f64>,
    selected_index: RwSignal<Option<usize>>,
    selected_match: RwSignal<Option<(PathBuf, usize, usize, usize)>>,
    internal_command: Listener<InternalCommand>,
    main_split: MainSplitData,
) -> Box<dyn View> {
    match row {
        SearchTreeRow::Folder {
            rel_path,
            name,
            expanded,
            match_count,
            level,
        } => {
            let toggle_path = rel_path.clone();
            let click_gs = global_search.clone();
            let row_key = format!("folder:{}", rel_path.display());
            let count_text = format!("({match_count})");

            Box::new(
                stack((
                    svg(move || {
                        config.get().ui_svg(if expanded {
                            LapceIcons::ITEM_OPENED
                        } else {
                            LapceIcons::ITEM_CLOSED
                        })
                    })
                    .style(move |s| {
                        let config = config.get();
                        let size = config.ui.icon_size() as f32;
                        s.margin_right(4.0)
                            .size(size, size)
                            .min_size(size, size)
                            .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    }),
                    svg(move || {
                        config.get().ui_svg(if expanded {
                            LapceIcons::DIRECTORY_OPENED
                        } else {
                            LapceIcons::DIRECTORY_CLOSED
                        })
                    })
                    .style(move |s| {
                        let config = config.get();
                        let size = config.ui.icon_size() as f32;
                        s.margin_right(6.0)
                            .size(size, size)
                            .min_size(size, size)
                            .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    }),
                    label(move || name.clone()).style(|s| s.text_ellipsis()),
                    label(move || count_text.clone()).style(move |s| {
                        s.margin_left(6.0)
                            .color(config.get().color(LapceColor::EDITOR_DIM))
                    }),
                ))
                .on_click_stop(move |_| {
                    click_gs.toggle_folder(&toggle_path);
                })
                .style(move |s| {
                    let config = config.get();
                    let indent = level as f32 * 16.0;
                    let line_h = ui_line_height.get();
                    let is_selected = is_row_selected(
                        &row_key,
                        selected_index,
                        &global_search.search_tree_rows,
                    );
                    s.width_pct(100.0)
                        .min_width_pct(100.0)
                        .height(line_h as f32)
                        .items_center()
                        .padding_left(indent + 8.0)
                        .apply_if(is_selected, |s| {
                            s.background(
                                config.color(LapceColor::PALETTE_CURRENT_BACKGROUND),
                            )
                        })
                        .hover(|s| {
                            s.cursor(CursorStyle::Pointer).background(
                                config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                        })
                }),
            )
        }
        SearchTreeRow::File {
            full_path,
            name,
            expanded,
            match_count,
            level,
        } => {
            let toggle_path = full_path.clone();
            let click_gs = global_search.clone();
            let icon_path = full_path.clone();
            let row_key = format!("file:{}", full_path.display());
            let count_text = format!("({match_count})");

            Box::new(
                stack((
                    svg(move || {
                        config.get().ui_svg(if expanded {
                            LapceIcons::ITEM_OPENED
                        } else {
                            LapceIcons::ITEM_CLOSED
                        })
                    })
                    .style(move |s| {
                        let config = config.get();
                        let size = config.ui.icon_size() as f32;
                        s.margin_right(4.0)
                            .size(size, size)
                            .min_size(size, size)
                            .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                    }),
                    file_icon_svg(config, move || icon_path.clone()),
                    label(move || name.clone()).style(|s| {
                        s.margin_left(6.0).text_ellipsis().max_width_pct(100.0)
                    }),
                    label(move || count_text.clone()).style(move |s| {
                        s.margin_left(6.0)
                            .color(config.get().color(LapceColor::EDITOR_DIM))
                    }),
                ))
                .on_click_stop(move |_| {
                    click_gs.toggle_file(&toggle_path);
                })
                .style(move |s| {
                    let config = config.get();
                    let indent = level as f32 * 16.0;
                    let line_h = ui_line_height.get();
                    let is_selected = is_row_selected(
                        &row_key,
                        selected_index,
                        &global_search.search_tree_rows,
                    );
                    s.width_pct(100.0)
                        .min_width_pct(100.0)
                        .height(line_h as f32)
                        .items_center()
                        .padding_left(indent + 8.0)
                        .apply_if(is_selected, |s| {
                            s.background(
                                config.color(LapceColor::PALETTE_CURRENT_BACKGROUND),
                            )
                        })
                        .hover(|s| {
                            s.cursor(CursorStyle::Pointer).background(
                                config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                        })
                }),
            )
        }
        SearchTreeRow::Match {
            full_path,
            search_match,
            level,
        } => {
            let click_path = full_path.clone();
            let double_click_path = full_path.clone();
            let selected_path = full_path.clone();
            let syntax_path = full_path.clone();
            let line_number = search_match.line;
            let start = search_match.start;
            let end = search_match.end;
            let line_content = search_match.line_content.clone();
            let syntax_line_content = search_match.line_content.clone();
            let click_gs = global_search.clone();
            let row_key = format!(
                "match:{}:{}:{}:{}",
                full_path.display(),
                search_match.line,
                search_match.start,
                search_match.end
            );
            Box::new(
                focus_text_with_syntax(
                    move || {
                        let config = config.get();
                        let content = if config.ui.trim_search_results_whitespace {
                            search_match.line_content.trim()
                        } else {
                            &search_match.line_content
                        };
                        format!("{}: {content}", search_match.line)
                    },
                    move || {
                        let config = config.get();
                        let mut offset = if config.ui.trim_search_results_whitespace
                        {
                            line_content.trim_start().len() as i32
                                - line_content.len() as i32
                        } else {
                            0
                        };
                        offset += line_number.to_string().len() as i32 + 2;

                        ((start as i32 + offset) as usize
                            ..(end as i32 + offset) as usize)
                            .collect()
                    },
                    move || config.get().color(LapceColor::EDITOR_FOCUS),
                    move || {
                        let config = config.get();
                        let trim = config.ui.trim_search_results_whitespace;
                        let prefix_len = line_number.to_string().len() + 2;
                        let trim_offset = if trim {
                            syntax_line_content.len()
                                - syntax_line_content.trim_start().len()
                        } else {
                            0
                        };

                        // Eagerly load the doc (triggers async file read if not cached)
                        let (doc, _new) =
                            main_split.get_doc(syntax_path.clone(), None);
                        // Track cache_rev so we re-run when syntax becomes available
                        let _rev = doc.cache_rev.get();
                        let line_styles =
                            doc.line_style(line_number.saturating_sub(1));
                        line_styles
                            .iter()
                            .filter_map(|ls| {
                                let color = ls
                                    .style
                                    .fg_color
                                    .as_ref()
                                    .and_then(|name| config.style_color(name))?;
                                let s = ls.start.saturating_sub(trim_offset)
                                    + prefix_len;
                                let e =
                                    ls.end.saturating_sub(trim_offset) + prefix_len;
                                if s < e { Some((s, e, color)) } else { None }
                            })
                            .collect()
                    },
                )
                .on_click_stop(move |_| {
                    click_gs.preview_focused.set(false);
                    click_gs.selected_match.set(Some((
                        click_path.clone(),
                        line_number,
                        start,
                        end,
                    )));
                    // Find our index in the rows
                    let rows = click_gs.search_tree_rows.get_untracked();
                    let row_key_inner = format!(
                        "match:{}:{}:{}:{}",
                        click_path.display(),
                        line_number,
                        start,
                        end
                    );
                    for (i, r) in rows.iter().enumerate() {
                        if r.key() == row_key_inner {
                            click_gs.selected_index.set(Some(i));
                            break;
                        }
                    }
                    click_gs.preview_match(click_path.clone(), line_number);
                })
                .on_double_click_stop(move |_| {
                    internal_command.send(InternalCommand::JumpToLocation {
                        location: EditorLocation {
                            path: double_click_path.clone(),
                            position: Some(EditorPosition::Line(
                                line_number.saturating_sub(1),
                            )),
                            scroll_offset: None,
                            same_editor_tab: false,
                        },
                    });
                })
                .style(move |s| {
                    let config = config.get();
                    let indent = level as f32 * 16.0;
                    let line_h = ui_line_height.get();
                    let is_match_selected = selected_match.get()
                        == Some((selected_path.clone(), line_number, start, end));
                    let is_row_sel = is_row_selected(
                        &row_key,
                        selected_index,
                        &global_search.search_tree_rows,
                    );
                    s.width_pct(100.0)
                        .min_width_pct(100.0)
                        .height(line_h as f32)
                        .items_center()
                        .padding_left(indent + 8.0)
                        .apply_if(is_match_selected || is_row_sel, |s| {
                            s.background(
                                config.color(LapceColor::PALETTE_CURRENT_BACKGROUND),
                            )
                        })
                        .hover(|s| {
                            s.cursor(CursorStyle::Pointer).background(
                                config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                            )
                        })
                }),
            )
        }
    }
}

/// Check if a row with the given key matches the currently selected index.
fn is_row_selected(
    row_key: &str,
    selected_index: RwSignal<Option<usize>>,
    search_tree_rows: &floem::reactive::Memo<Vec<SearchTreeRow>>,
) -> bool {
    if let Some(idx) = selected_index.get() {
        let rows = search_tree_rows.get();
        if let Some(row) = rows.get(idx) {
            return row.key() == row_key;
        }
    }
    false
}

/// The preview editor pane, shown to the right of the results list when a match
/// is selected. Clicking into it sets preview_focused, which enables full editor
/// keybindings (cursor movement, selection, etc.) while the preview is active.
fn search_preview_editor(
    workspace_data: Rc<WorkspaceData>,
    config: ReadSignal<Arc<LapceConfig>>,
    has_preview: RwSignal<bool>,
    preview_focused: RwSignal<bool>,
) -> impl View {
    let global_search = workspace_data.global_search.clone();
    let workspace = workspace_data.workspace.clone();
    let preview_editor = create_rw_signal(global_search.preview_editor.clone());

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
                .border_left(1.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .size_full()
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
    )
    .style(move |s| {
        s.height_pct(100.0)
            .min_width(0)
            .flex_basis(0)
            .flex_grow(1.0)
            .display(if has_preview.get() {
                Display::Flex
            } else {
                Display::None
            })
    })
}
