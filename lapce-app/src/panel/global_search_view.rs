use std::{path::PathBuf, rc::Rc, sync::Arc};

use floem::{
    View,
    event::EventListener,
    reactive::{ReadSignal, RwSignal, SignalGet, SignalUpdate, create_rw_signal},
    style::{CursorStyle, Display},
    views::{Decorators, container, scroll, stack, svg, virtual_stack},
};

use super::position::PanelPosition;
use crate::{
    command::InternalCommand,
    config::{LapceConfig, color::LapceColor, icon::LapceIcons},
    editor::location::{EditorLocation, EditorPosition},
    editor::view::editor_container_view,
    focus_text::focus_text,
    global_search::{GlobalSearchData, SearchMatchData},
    listener::Listener,
    workspace::LapceWorkspace,
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
    let workspace = global_search.common.workspace.clone();
    let internal_command = global_search.common.internal_command;
    let has_preview = global_search.has_preview;
    let preview_focused = global_search.preview_focused;

    stack((
        search_result(workspace, global_search, internal_command, config).style(
            move |s| {
                let w = if has_preview.get() { 50.0 } else { 100.0 };
                s.width_pct(w).height_pct(100.0)
            },
        ),
        search_preview_editor(workspace_data, config, has_preview, preview_focused),
    ))
    .style(|s| s.absolute().size_pct(100.0, 100.0).flex_row())
    .debug_name("Global Search Panel")
}

/// Renders the hierarchical search results. Uses a nested virtual_stack pattern:
/// the outer stack has variable-height items (one per file, height depends on
/// expanded match count), and each file's inner stack virtualizes the individual
/// matches. Single-click on a match previews it in the right pane; double-click
/// navigates to it in the main editor.
fn search_result(
    workspace: Arc<LapceWorkspace>,
    global_search_data: GlobalSearchData,
    internal_command: Listener<InternalCommand>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let ui_line_height = global_search_data.common.ui_line_height;
    let selected_match = global_search_data.selected_match;
    let item_search_data = global_search_data.clone();
    container({
        scroll({
            virtual_stack(
                move || global_search_data.clone(),
                move |(path, _)| path.to_owned(),
                move |(path, match_data)| {
                    let item_search_data = item_search_data.clone();
                    let full_path = path.clone();
                    let path = if let Some(workspace_path) = workspace.path.as_ref()
                    {
                        path.strip_prefix(workspace_path)
                            .unwrap_or(&full_path)
                            .to_path_buf()
                    } else {
                        path
                    };
                    let file_name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();

                    let folder = path
                        .parent()
                        .map(|s| crate::path::display_path(s))
                        .unwrap_or_default();

                    let expanded = match_data.expanded;

                    stack((
                        stack((
                            svg(move || {
                                config.get().ui_svg(if expanded.get() {
                                    LapceIcons::ITEM_OPENED
                                } else {
                                    LapceIcons::ITEM_CLOSED
                                })
                            })
                            .style(move |s| {
                                let config = config.get();
                                let size = config.ui.icon_size() as f32;
                                s.margin_left(10.0)
                                    .margin_right(6.0)
                                    .size(size, size)
                                    .min_size(size, size)
                                    .color(
                                        config.color(LapceColor::LAPCE_ICON_ACTIVE),
                                    )
                            }),
                            crate::file_icon::file_icon_with_name(
                                config,
                                move || path.clone(),
                                move || file_name.clone(),
                                move || folder.clone(),
                            ),
                        ))
                        .on_click_stop(move |_| {
                            expanded.update(|expanded| *expanded = !*expanded);
                        })
                        .style(move |s| {
                            s.width_pct(100.0)
                                .min_width_pct(100.0)
                                .items_center()
                                .hover(|s| {
                                    s.cursor(CursorStyle::Pointer).background(
                                        config.get().color(
                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                        ),
                                    )
                                })
                        }),
                        virtual_stack(
                            move || {
                                if expanded.get() {
                                    match_data.matches.get()
                                } else {
                                    im::Vector::new()
                                }
                            },
                            |m| (m.line, m.start, m.end),
                            move |m| {
                                let click_path = full_path.clone();
                                let double_click_path = full_path.clone();
                                let selected_path = full_path.clone();
                                let line_number = m.line;
                                let start = m.start;
                                let end = m.end;
                                let line_content = m.line_content.clone();
                                let click_search = item_search_data.clone();

                                focus_text(
                                    move || {
                                        let config = config.get();
                                        let content = if config
                                            .ui
                                            .trim_search_results_whitespace
                                        {
                                            m.line_content.trim()
                                        } else {
                                            &m.line_content
                                        };
                                        format!("{}: {content}", m.line,)
                                    },
                                    move || {
                                        let config = config.get();
                                        let mut offset = if config
                                            .ui
                                            .trim_search_results_whitespace
                                        {
                                            line_content.trim_start().len() as i32
                                                - line_content.len() as i32
                                        } else {
                                            0
                                        };
                                        offset +=
                                            line_number.to_string().len() as i32 + 2;

                                        ((start as i32 + offset) as usize
                                            ..(end as i32 + offset) as usize)
                                            .collect()
                                    },
                                    move || {
                                        config.get().color(LapceColor::EDITOR_FOCUS)
                                    },
                                )
                                .style(move |s| {
                                    let config = config.get();
                                    let icon_size = config.ui.icon_size() as f32;
                                    let is_selected = selected_match.get()
                                        == Some((
                                            selected_path.clone(),
                                            line_number,
                                            start,
                                            end,
                                        ));
                                    s.margin_left(10.0 + icon_size + 6.0)
                                        .apply_if(is_selected, |s| {
                                            s.background(config.color(
                                                LapceColor::PALETTE_CURRENT_BACKGROUND,
                                            ))
                                        })
                                        .hover(|s| {
                                            s.cursor(CursorStyle::Pointer)
                                                .background(config.color(
                                                LapceColor::PANEL_HOVERED_BACKGROUND,
                                            ))
                                        })
                                })
                                .on_click_stop(move |_| {
                                    click_search.preview_focused.set(false);
                                    click_search.selected_match.set(Some((
                                        click_path.clone(),
                                        line_number,
                                        start,
                                        end,
                                    )));
                                    click_search
                                        .preview_match(click_path.clone(), line_number);
                                })
                                .on_double_click_stop(move |_| {
                                    internal_command.send(
                                        InternalCommand::JumpToLocation {
                                            location: EditorLocation {
                                                path: double_click_path.clone(),
                                                position: Some(
                                                    EditorPosition::Line(
                                                        line_number
                                                            .saturating_sub(1),
                                                    ),
                                                ),
                                                scroll_offset: None,
                                                same_editor_tab: false,
                                            },
                                        },
                                    );
                                })
                            },
                        )
                        .item_size_fixed(move || ui_line_height.get())
                        .style(|s| s.flex_col()),
                    ))
                    .style(|s| s.flex_col())
                },
            )
            .item_size_fn(|(_, match_data): &(PathBuf, SearchMatchData)| {
                match_data.height()
            })
            .style(|s| s.flex_col().min_width_pct(100.0).line_height(1.8))
        })
        .style(|s| s.absolute().size_pct(100.0, 100.0))
    })
    .style(|s| s.size_pct(100.0, 100.0))
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
        s.width_pct(50.0).height_pct(100.0).min_width(0.0).display(
            if has_preview.get() {
                Display::Flex
            } else {
                Display::None
            },
        )
    })
}
