use std::{path::PathBuf, rc::Rc, sync::Arc};

use floem::{
    View,
    event::EventListener,
    peniko::Color,
    reactive::{
        ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith, create_rw_signal,
    },
    style::{CursorStyle, Display},
    views::{
        Decorators, container, dyn_stack, label, resizable::resizable, scroll,
        stack, svg, virtual_stack,
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
    panel::kind::PanelKind,
    search_tabs::SearchTabsData,
    workspace_data::{Focus, WorkspaceData},
};

/// The search panel shows a tab header bar at the top, with one tab per search query.
/// Below the tab bar is a 50/50 horizontal split: hierarchical results on the left,
/// preview editor on the right. The content area is driven by the active tab's
/// GlobalSearchData.
pub fn global_search_panel(
    workspace_data: Rc<WorkspaceData>,
    _position: PanelPosition,
) -> impl View {
    let search_tabs = workspace_data.search_tabs.clone();
    let config = workspace_data.common.config;

    stack((
        // Tab header bar
        search_tab_header(search_tabs.clone(), config),
        // Content area: active tab's results + preview
        search_tab_content(workspace_data, search_tabs, config),
    ))
    .style(|s| s.absolute().size_pct(100.0, 100.0).flex_col())
    .debug_name("Global Search Panel")
}

/// Renders the horizontal tab header bar for search tabs.
/// Styled to match editor tab headers (see `editor_tab_header()` in `app/editor_tabs.rs`).
fn search_tab_header(
    search_tabs: SearchTabsData,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let tabs = search_tabs.tabs;
    let active_tab = search_tabs.active_tab;
    let internal_command = search_tabs.common.internal_command;

    stack((
        // Scrollable tab list
        scroll(
            dyn_stack(
                move || {
                    let tabs_vec = tabs.get();
                    tabs_vec
                        .iter()
                        .enumerate()
                        .map(|(i, gs)| {
                            let pattern = gs.pattern_text();
                            (i, pattern)
                        })
                        .collect::<im::Vector<_>>()
                },
                move |(i, _)| *i,
                move |(i, pattern)| {
                    let search_tabs = search_tabs.clone();
                    let close_command = internal_command;
                    let tab_index = i;
                    let tab_hovered = create_rw_signal(false);
                    let close_hovered = create_rw_signal(false);
                    let pattern_display = if pattern.len() > 30 {
                        format!("{}...", &pattern[..27])
                    } else {
                        pattern.clone()
                    };

                    // Tab content: search icon + label + close button
                    let tab_content = stack((
                        // Search icon
                        svg(move || config.get().ui_svg(LapceIcons::SEARCH)).style(
                            move |s| {
                                let config = config.get();
                                let size = config.ui.icon_size() as f32;
                                s.size(size, size).color(
                                    config.color(LapceColor::LAPCE_ICON_ACTIVE),
                                )
                            },
                        ),
                        // Pattern text
                        label(move || pattern_display.clone()).style(|s| {
                            s.text_ellipsis().max_width(200.0).selectable(false)
                        }),
                        // Close button (X) — visible only when active or hovered
                        container(
                            svg(move || config.get().ui_svg(LapceIcons::CLOSE))
                                .style(move |s| {
                                    let config = config.get();
                                    let size = config.ui.icon_size() as f32 - 2.0;
                                    let is_active = active_tab.get() == tab_index;
                                    let is_tab_hovered = tab_hovered.get();
                                    let visible = is_active || is_tab_hovered;
                                    s.size(size, size)
                                        .apply_if(!visible, |s| {
                                            s.color(Color::TRANSPARENT)
                                        })
                                        .apply_if(visible, |s| {
                                            s.color(config.color(
                                                LapceColor::LAPCE_ICON_ACTIVE,
                                            ))
                                        })
                                }),
                        )
                        .on_click_stop(move |_| {
                            close_command.send(InternalCommand::CloseSearchTab {
                                index: tab_index,
                            });
                        })
                        .on_event_stop(EventListener::PointerDown, |_| {})
                        .on_event_stop(EventListener::PointerEnter, move |_| {
                            close_hovered.set(true);
                        })
                        .on_event_stop(EventListener::PointerLeave, move |_| {
                            close_hovered.set(false);
                        })
                        .style(move |s| {
                            s.padding(2.0)
                                .border_radius(4.0)
                                .cursor(CursorStyle::Pointer)
                                .hover(|s| {
                                    s.background(
                                        config.get().color(
                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                        ),
                                    )
                                })
                        }),
                    ))
                    .style(|s| s.items_center().padding_horiz(6.).gap(6.));

                    // Tab wrapper with editor tab styling
                    tab_content
                        .on_click_stop(move |_| {
                            search_tabs.activate_tab(tab_index);
                        })
                        .on_event_cont(EventListener::PointerEnter, move |_| {
                            tab_hovered.set(true);
                        })
                        .on_event_cont(EventListener::PointerLeave, move |_| {
                            tab_hovered.set(false);
                        })
                        .style(move |s| {
                            let config = config.get();
                            let is_active = active_tab.get() == tab_index;
                            let accent =
                                config.color(LapceColor::LAPCE_TAB_ACTIVE_UNDERLINE);
                            let h = (config.ui.header_height()) as f32 * 0.7;
                            s.items_center()
                                .height(h)
                                .cursor(CursorStyle::Pointer)
                                .border_radius(LapceLayout::BORDER_RADIUS)
                                .margin_top(5.0)
                                .margin_bottom(3.0)
                                .margin_horiz(2.0)
                                .border(1.0)
                                .border_color(Color::TRANSPARENT)
                                .apply_if(is_active, |s| {
                                    s.background(accent.multiply_alpha(0.15))
                                        .border_color(accent.multiply_alpha(
                                            LapceLayout::SHADOW_ALPHA,
                                        ))
                                        .color(config.color(
                                            LapceColor::LAPCE_TAB_ACTIVE_FOREGROUND,
                                        ))
                                })
                                .apply_if(!is_active, |s| {
                                    s.color(config.color(
                                        LapceColor::LAPCE_TAB_INACTIVE_FOREGROUND,
                                    ))
                                    .hover(
                                        |s| {
                                            s.background(
                                                config.color(
                                                    LapceColor::HOVER_BACKGROUND,
                                                ),
                                            )
                                        },
                                    )
                                })
                        })
                },
            )
            .style(|s| s.height_pct(100.0)),
        )
        .style(|s| s.flex_grow(1.0).min_width(0).height_pct(100.0)),
        // Close all button
        {
            let internal_command = internal_command;
            container(svg(move || config.get().ui_svg(LapceIcons::CLOSE)).style(
                move |s| {
                    let config = config.get();
                    let size = config.ui.icon_size() as f32 - 2.0;
                    s.size(size, size)
                        .color(config.color(LapceColor::LAPCE_ICON_ACTIVE))
                },
            ))
            .on_click_stop(move |_| {
                internal_command.send(InternalCommand::CloseAllSearchTabs);
            })
            .style(move |s| {
                s.padding(2.0)
                    .border_radius(4.0)
                    .margin_horiz(6.0)
                    .cursor(CursorStyle::Pointer)
                    .hover(|s| {
                        s.background(
                            config.get().color(LapceColor::PANEL_HOVERED_BACKGROUND),
                        )
                    })
            })
        },
    ))
    .style(move |s| {
        let config = config.get();
        let has_tabs = !tabs.with(|t| t.is_empty());
        let h = config.ui.header_height() as f32;
        s.width_pct(100.0)
            .items_center()
            .max_width_full()
            .padding_horiz(4.0)
            .height(h)
            .min_height(h)
            .max_height(h)
            .display(if has_tabs {
                Display::Flex
            } else {
                Display::None
            })
    })
}

/// Renders the content area for the active search tab.
fn search_tab_content(
    workspace_data: Rc<WorkspaceData>,
    search_tabs: SearchTabsData,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let tabs = search_tabs.tabs;
    let active_tab = search_tabs.active_tab;

    // We use a dynamic container that re-renders when the active tab changes.
    // The key insight: we need to create the view reactively based on active_tab.
    container(
        dyn_stack(
            move || {
                let active = active_tab.get();
                let tab_data = tabs.with(|t| t.get(active).cloned());
                // Return a single-element vec if we have a tab, empty otherwise
                tab_data.into_iter().collect::<Vec<_>>()
            },
            // Use the active tab index as key so we rebuild on tab switch
            move |_| active_tab.get_untracked(),
            move |gs| {
                let has_preview = gs.has_preview;
                let preview_focused = gs.preview_focused;

                resizable((
                    search_result(gs.clone(), config).style(move |s| {
                        s.height_pct(100.0)
                            .min_width(0)
                            .flex_basis(0)
                            .flex_grow(1.0)
                    }),
                    search_preview_editor(
                        workspace_data.clone(),
                        gs,
                        config,
                        has_preview,
                        preview_focused,
                    ),
                ))
                .style(|s| s.size_pct(100.0, 100.0).flex_row())
            },
        )
        .style(|s| s.size_pct(100.0, 100.0)),
    )
    .style(|s| s.flex_grow(1.0).min_height(0).width_pct(100.0))
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
    global_search: GlobalSearchData,
    config: ReadSignal<Arc<LapceConfig>>,
    has_preview: RwSignal<bool>,
    preview_focused: RwSignal<bool>,
) -> impl View {
    let focus = global_search.common.focus;
    let workspace = workspace_data.workspace.clone();
    let preview_editor = create_rw_signal(global_search.preview_editor.clone());

    container(
        container(editor_container_view(
            workspace_data,
            workspace,
            move |tracked: bool| {
                let f = if tracked {
                    focus.get()
                } else {
                    focus.get_untracked()
                };
                let pf = if tracked {
                    preview_focused.get()
                } else {
                    preview_focused.get_untracked()
                };
                matches!(f, Focus::Panel(PanelKind::Search)) && pf
            },
            preview_editor,
        ))
        .on_event_cont(EventListener::PointerDown, move |_| {
            preview_focused.set(true);
            focus.set(Focus::Panel(PanelKind::Search));
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
