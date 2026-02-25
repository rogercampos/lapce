use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use floem::{
    View,
    event::{Event, EventListener},
    kurbo::Rect,
    peniko::Color,
    reactive::{
        ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith, create_rw_signal,
    },
    style::{AlignItems, CursorStyle, Position, Style},
    views::{
        Container, Decorators, container, dyn_stack, h_stack, label, scroll, stack,
        svg, text, virtual_stack,
    },
};
use lapce_core::selection::Selection;
use lapce_rpc::{
    core::GitFileStatus,
    file::{FileNodeViewData, FileNodeViewKind, Naming},
};
use lapce_xi_rope::Rope;

use super::{data::FileExplorerData, node::FileNodeVirtualList};
use crate::{
    app::clickable_icon,
    command::InternalCommand,
    config::{
        LapceConfig, color::LapceColor, icon::LapceIcons, layout::LapceLayout,
    },
    doc::DocContent,
    editor_tab::{EditorTabChild, EditorTabData},
    panel::{
        data::PanelSection, kind::PanelKind, position::PanelPosition,
        view::PanelBuilder,
    },
    text_input::TextInputBuilder,
    workspace_data::{Focus, WorkspaceData},
};

/// Helper: returns a star icon view for a first-level directory item.
/// The icon is always visible when the folder is starred, and only visible
/// on hover when unstarred. Clicking toggles the starred state.
fn star_icon_view(
    data: FileExplorerData,
    path: PathBuf,
    is_first_level_dir: bool,
    row_hovered: RwSignal<bool>,
) -> impl View {
    let config = data.common.config;
    let starred = data.starred;

    let path_for_svg = path.clone();
    let path_for_style = path.clone();
    let path_for_outer_style = path.clone();
    let path_for_toggle = path;

    container(
        svg(move || {
            let config = config.get();
            if starred.with(|set| set.contains(&path_for_svg)) {
                config.ui_svg(LapceIcons::STAR_FULL)
            } else {
                config.ui_svg(LapceIcons::STAR_EMPTY)
            }
        })
        .style(move |s| {
            let config = config.get();
            let size = config.ui.icon_size() as f32 + 4.0;
            let is_starred = starred.with(|set| set.contains(&path_for_style));
            let yellow = Color::from_rgba8(234, 179, 8, 255);
            s.size(size, size).color(if is_starred {
                yellow
            } else {
                config.color(LapceColor::PANEL_FOREGROUND)
            })
        }),
    )
    .on_click_stop(move |_| {
        data.toggle_star(&path_for_toggle);
    })
    .on_event_stop(EventListener::PointerDown, |_| {
        // Prevent the click from propagating to the row's click handler
        // which would toggle expand/collapse on the directory.
    })
    .style(move |s| {
        let is_starred = starred.with(|set| set.contains(&path_for_outer_style));
        s.margin_left(4.0)
            .margin_right(4.0)
            .cursor(CursorStyle::Pointer)
            // Hide entirely if not a first-level dir
            .apply_if(!is_first_level_dir, |s| s.hide())
            // Show only on hover for unstarred, always show when starred
            .apply_if(
                is_first_level_dir && !is_starred && !row_hovered.get(),
                |s| s.hide(),
            )
    })
}

/// Blends `foreground` with `background`.
///
/// Uses the alpha channel from `foreground` - if `foreground` is opaque, `foreground` will be
/// returned unchanged.
///
/// The result is always opaque regardless of the transparency of the inputs.
fn blend_colors(background: Color, foreground: Color) -> Color {
    let background = background.to_rgba8();
    let foreground = foreground.to_rgba8();
    let a: u16 = foreground.a.into();
    let [r, g, b] = [
        [background.r, foreground.r],
        [background.g, foreground.g],
        [background.b, foreground.b],
    ]
    .map(|x| x.map(u16::from))
    .map(|[b, f]| (a * f + (255 - a) * b) / 255)
    .map(|x| x as u8);

    Color::from_rgba8(r, g, b, 255)
}

/// Builds the complete file explorer panel with two foldable sections:
/// "Open Editors" (showing tabs from all editor groups) and "File Explorer"
/// (the tree view). The "Open Editors" section has a fixed height and can
/// be hidden via config.
pub fn file_explorer_panel(
    workspace_data: Rc<WorkspaceData>,
    position: PanelPosition,
) -> impl View {
    let config = workspace_data.common.config;
    let data = workspace_data.file_explorer.clone();
    let git_file_statuses = workspace_data.git_file_statuses;

    let file_explorer_header = {
        let wtd = workspace_data.clone();
        h_stack((
            text("File Explorer").style(move |s| s.selectable(false).flex_grow(1.0)),
            clickable_icon(
                || LapceIcons::LOCATE_FILE,
                move || {
                    if let Some(editor_data) =
                        wtd.main_split.active_editor.get_untracked()
                    {
                        if let Some(path) = editor_data.try_doc().and_then(|doc| {
                            if let DocContent::File { path, .. } =
                                doc.content.get_untracked()
                            {
                                Some(path)
                            } else {
                                None
                            }
                        }) {
                            wtd.file_explorer.reveal_in_file_tree(path);
                        }
                    }
                },
                || false,
                || false,
                || "Reveal Active File in File Explorer",
                config,
            ),
        ))
        .style(|s| s.width_full().align_items(AlignItems::Center))
    };

    PanelBuilder::new(config, position)
        .add_height_style(
            "Open Editors",
            150.0,
            container(open_editors_view(workspace_data.clone()))
                .style(|s| s.size_full()),
            workspace_data.panel.section_open(PanelSection::OpenEditor),
            move |s| s.apply_if(!config.get().ui.open_editors_visible, |s| s.hide()),
        )
        .add_with_header(
            file_explorer_header,
            container(file_explorer_view(data, git_file_statuses))
                .style(|s| s.size_full()),
            workspace_data
                .panel
                .section_open(PanelSection::FileExplorer),
        )
        .build()
        .debug_name("File Explorer Panel")
}

/// Initialize the file explorer's naming (renaming, creating, etc.) editor with the given path.
/// Selects just the stem portion (before the extension) so the user can quickly type
/// a new name without re-typing the extension. Handles dotfiles (e.g., ".gitignore")
/// by treating the leading dot as part of the stem.
fn initialize_naming_editor_with_path(data: &FileExplorerData, path: &Path) {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let selection_end = {
        let without_leading_dot = file_name.strip_prefix('.').unwrap_or(&file_name);
        let idx = without_leading_dot
            .find('.')
            .unwrap_or(without_leading_dot.len());

        idx + file_name.len() - without_leading_dot.len()
    };

    initialize_naming_editor(data, &file_name, Some(selection_end));
}

fn initialize_naming_editor(
    data: &FileExplorerData,
    text: &str,
    selection_end: Option<usize>,
) {
    let text = Rope::from(text);
    let selection_end = selection_end.unwrap_or(text.len());

    let doc = data.naming_editor_data.doc();
    doc.reload(text, true);
    data.naming_editor_data
        .cursor()
        .update(|cursor| cursor.set_insert(Selection::region(0, selection_end)));

    data.naming
        .update(|naming| naming.set_editor_needs_reset(false));
}

fn file_node_text_color(
    config: ReadSignal<Arc<LapceConfig>>,
    git_file_statuses: RwSignal<HashMap<PathBuf, GitFileStatus>>,
    path: &Path,
    is_dir: bool,
    is_excluded: bool,
) -> Color {
    if is_excluded {
        return config.get().color(LapceColor::PANEL_EXCLUDED_FOREGROUND);
    }
    if is_dir {
        return config.get().color(LapceColor::PANEL_FOREGROUND);
    }
    let status = git_file_statuses.with(|statuses| statuses.get(path).cloned());
    let cfg = config.get();
    match status {
        Some(GitFileStatus::Modified | GitFileStatus::Renamed) => {
            cfg.color(LapceColor::SOURCE_CONTROL_MODIFIED)
        }
        Some(GitFileStatus::Added) => cfg.color(LapceColor::SOURCE_CONTROL_ADDED),
        Some(GitFileStatus::Deleted) => {
            cfg.color(LapceColor::SOURCE_CONTROL_REMOVED)
        }
        Some(GitFileStatus::Untracked) => {
            cfg.color(LapceColor::SOURCE_CONTROL_UNTRACKED)
        }
        Some(GitFileStatus::Conflicted) => {
            cfg.color(LapceColor::SOURCE_CONTROL_CONFLICTED)
        }
        Some(GitFileStatus::Ignored) => {
            cfg.color(LapceColor::SOURCE_CONTROL_IGNORED)
        }
        None => cfg.color(LapceColor::PANEL_FOREGROUND),
    }
}

fn file_node_text_view(
    data: FileExplorerData,
    node: FileNodeViewData,
    git_file_statuses: RwSignal<HashMap<PathBuf, GitFileStatus>>,
    is_excluded: bool,
) -> impl View {
    let config = data.common.config;
    let ui_line_height = data.common.ui_line_height;

    match node.kind.clone() {
        FileNodeViewKind::Path(path) => {
            if node.is_root {
                let file = path.clone();
                let path_for_display = path.clone();
                container((
                    label(move || {
                        file.file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default()
                    })
                    .style(move |s| {
                        s.height(ui_line_height.get())
                            .color(file_node_text_color(
                                config,
                                git_file_statuses,
                                &path,
                                true,
                                is_excluded,
                            ))
                            .font_bold()
                            .font_size(config.get().ui.font_size() as f32 + 1.0)
                            .padding_right(5.0)
                            .selectable(false)
                    }),
                    label(move || crate::path::display_path(&path_for_display))
                        .style(move |s| {
                            s.height(ui_line_height.get())
                                .color(
                                    config
                                        .get()
                                        .color(LapceColor::PANEL_FOREGROUND_DIM),
                                )
                                .font_size(config.get().ui.font_size() as f32 - 1.0)
                                .selectable(false)
                        }),
                ))
            } else {
                let is_dir = node.is_dir;
                let path_for_color = path.clone();
                container(
                    label(move || {
                        path.file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default()
                    })
                    .style(move |s| {
                        s.height(ui_line_height.get())
                            .color(file_node_text_color(
                                config,
                                git_file_statuses,
                                &path_for_color,
                                is_dir,
                                is_excluded,
                            ))
                            .font_size(config.get().ui.font_size() as f32 + 1.0)
                            .selectable(false)
                    }),
                )
            }
        }
        FileNodeViewKind::Renaming { path, err } => {
            if data.naming.with_untracked(Naming::editor_needs_reset) {
                initialize_naming_editor_with_path(&data, &path);
            }

            file_node_input_view(data, err.clone())
        }
        FileNodeViewKind::Naming { err } => {
            if data.naming.with_untracked(Naming::editor_needs_reset) {
                initialize_naming_editor(&data, "", None);
            }

            file_node_input_view(data, err.clone())
        }
        FileNodeViewKind::Duplicating { source, err } => {
            if data.naming.with_untracked(Naming::editor_needs_reset) {
                initialize_naming_editor_with_path(&data, &source);
            }

            file_node_input_view(data, err.clone())
        }
    }
}

/// Input used for naming a file/directory. When an error is present (e.g., the
/// target path already exists), it shows a floating error label below the input
/// using absolute positioning and z_index to overlay on top of subsequent tree items.
fn file_node_input_view(data: FileExplorerData, err: Option<String>) -> Container {
    let ui_line_height = data.common.ui_line_height;

    let naming_editor_data = data.naming_editor_data.clone();
    let text_input_file_explorer_data = data.clone();
    let focus = data.common.focus;
    let config = data.common.config;

    let is_focused = move || {
        focus.with_untracked(|focus| focus == &Focus::Panel(PanelKind::FileExplorer))
    };
    let text_input_view = TextInputBuilder::new()
        .is_focused(is_focused)
        .key_focus(text_input_file_explorer_data)
        .build_editor(naming_editor_data.clone())
        .on_event_stop(EventListener::FocusLost, move |_| {
            data.finish_naming();
            data.naming.set(Naming::None);
        })
        .style(move |s| {
            s.width_full()
                .height(ui_line_height.get())
                .padding(0.0)
                .margin(0.0)
                .border_radius(LapceLayout::BORDER_RADIUS)
                .border(1.0)
                .border_color(config.get().color(LapceColor::LAPCE_BORDER))
        });

    let text_input_id = text_input_view.id();
    text_input_id.request_focus();

    if let Some(err) = err {
        container(
            stack((
                text_input_view,
                label(move || err.clone()).style(move |s| {
                    let config = config.get();

                    let editor_background_color =
                        config.color(LapceColor::PANEL_CURRENT_BACKGROUND);
                    let error_background_color =
                        config.color(LapceColor::ERROR_LENS_ERROR_BACKGROUND);

                    let background_color = blend_colors(
                        editor_background_color,
                        error_background_color,
                    );

                    s.position(Position::Absolute)
                        .inset_top(ui_line_height.get())
                        .width_full()
                        .color(config.color(LapceColor::ERROR_LENS_ERROR_FOREGROUND))
                        .background(background_color)
                        .z_index(100)
                }),
            ))
            .style(|s| s.flex_grow(1.0)),
        )
    } else {
        container(text_input_view)
    }
    .style(move |s| s.width_full())
}

/// The main file tree view. Uses a virtual_stack for efficient rendering of
/// potentially thousands of file nodes. The FileNodeVirtualList adapter flattens
/// the recursive tree into a linear list for the virtual stack, using the
/// pre-computed children_open_count for O(1) total_len(). Each node renders
/// differently based on its kind: Path (normal file/dir), Renaming, Naming
/// (new file), or Duplicating.
fn file_explorer_view(
    data: FileExplorerData,
    git_file_statuses: RwSignal<HashMap<PathBuf, GitFileStatus>>,
) -> impl View {
    let root = data.root;
    let starred = data.starred;
    let ui_line_height = data.common.ui_line_height;
    let config = data.common.config;
    let naming = data.naming;
    let scroll_to_line = data.scroll_to_line;
    let select = data.select;
    let secondary_click_data = data.clone();
    let scroll_rect = create_rw_signal(Rect::ZERO);
    let workspace_path = data.common.workspace.path.clone();

    scroll(
        virtual_stack(
            move || {
                FileNodeVirtualList::new(
                    root.get(),
                    data.naming.get(),
                    starred.get(),
                )
            },
            move |node| (node.kind.clone(), node.is_dir, node.open, node.level),
            move |node| {
                let level = node.level;
                let data = data.clone();
                let click_data = data.clone();
                let double_click_data = data.clone();
                let secondary_click_data = data.clone();
                let aux_click_data = data.clone();
                let kind = node.kind.clone();
                let open = node.open;
                let is_dir = node.is_dir;

                let is_excluded = {
                    let excluded_dirs =
                        &config.get_untracked().core.excluded_directories;
                    if excluded_dirs.is_empty() {
                        false
                    } else if let (Some(ws), Some(node_path)) =
                        (&workspace_path, kind.path())
                    {
                        if let Ok(rel) = node_path.strip_prefix(ws) {
                            excluded_dirs.iter().any(|dir| rel.starts_with(dir))
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                let view =
                    {
                        // level 2 = first-level children of the workspace root (starrable)
                        let is_first_level_dir = is_dir && level == 2;
                        let star_path =
                            kind.path().map(|p| p.to_path_buf()).unwrap_or_default();
                        let star_data = data.clone();
                        let row_hovered = create_rw_signal(false);

                        stack((
                            svg(move || {
                                let config = config.get();
                                let svg_str = match open {
                                    true => LapceIcons::ITEM_OPENED,
                                    false => LapceIcons::ITEM_CLOSED,
                                };
                                config.ui_svg(svg_str)
                            })
                            .style(move |s| {
                                let config = config.get();
                                let size = config.ui.icon_size() as f32;

                                let color = if !is_dir {
                                    Color::TRANSPARENT
                                } else if is_excluded {
                                    config
                                        .color(LapceColor::PANEL_EXCLUDED_FOREGROUND)
                                } else {
                                    config.color(LapceColor::LAPCE_ICON_ACTIVE)
                                };
                                s.size(size, size)
                                    .flex_shrink(0.0)
                                    .margin_left(4.0)
                                    .color(color)
                            }),
                            {
                                let kind = kind.clone();
                                let kind_for_style = kind.clone();
                                // TODO: use the current naming input as the path for the file svg
                                svg(move || {
                                    let config = config.get();
                                    if is_dir {
                                        let svg_str = match open {
                                            true => LapceIcons::DIRECTORY_OPENED,
                                            false => LapceIcons::DIRECTORY_CLOSED,
                                        };
                                        config.ui_svg(svg_str)
                                    } else if let Some(path) = kind.path() {
                                        config.file_svg(path).0
                                    } else {
                                        config.ui_svg(LapceIcons::FILE)
                                    }
                                })
                                .style(move |s| {
                                    let config = config.get();
                                    let base_size = config.ui.icon_size() as f32;
                                    let size = if is_dir {
                                        (base_size * 1.25).round()
                                    } else {
                                        base_size
                                    };

                                    s.size(size, size)
                                        .flex_shrink(0.0)
                                        .margin_horiz(6.0)
                                        .apply_if(is_excluded, |s| {
                                            s.color(config.color(
                                            LapceColor::PANEL_EXCLUDED_FOREGROUND,
                                        ))
                                        })
                                        .apply_if(is_dir && !is_excluded, |s| {
                                            s.color(config.color(
                                                LapceColor::LAPCE_ICON_ACTIVE,
                                            ))
                                        })
                                        .apply_if(!is_dir && !is_excluded, |s| {
                                            s.apply_opt(
                                                kind_for_style.path().and_then(
                                                    |p| config.file_svg(p).1,
                                                ),
                                                Style::color,
                                            )
                                        })
                                })
                            },
                            file_node_text_view(
                                data,
                                node,
                                git_file_statuses,
                                is_excluded,
                            ),
                            // Spacer to push star icon to the right
                            container(star_icon_view(
                                star_data,
                                star_path,
                                is_first_level_dir,
                                row_hovered,
                            ))
                            .style(|s| s.flex_grow(1.0).justify_end()),
                        ))
                        .on_event_cont(EventListener::PointerEnter, move |_| {
                            row_hovered.set(true);
                        })
                        .on_event_cont(
                            EventListener::PointerLeave,
                            move |_| {
                                row_hovered.set(false);
                            },
                        )
                    }
                    .style({
                        let kind = kind.clone();
                        move |s| {
                            s.padding_right(15.0)
                                .min_width_full()
                                .padding_left((level * 16) as f32)
                                .margin_horiz(4.0)
                                .border_radius(4.0)
                                .align_items(AlignItems::Center)
                                .apply_if(is_excluded, |s| {
                                    s.background(config.get().color(
                                        LapceColor::PANEL_EXCLUDED_BACKGROUND,
                                    ))
                                })
                                .hover(|s| {
                                    s.background(
                                        config.get().color(
                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                        ),
                                    )
                                    .cursor(CursorStyle::Pointer)
                                })
                                .apply_if(
                                    select
                                        .get()
                                        .map(|x| x == kind)
                                        .unwrap_or_default(),
                                    |x| {
                                        x.background(config.get().color(
                                            LapceColor::PANEL_CURRENT_BACKGROUND,
                                        ))
                                    },
                                )
                        }
                    })
                    .debug_name("file item");

                // Only handle click events if we are not naming the file node
                if let FileNodeViewKind::Path(path) = &kind {
                    let click_path = path.clone();
                    let double_click_path = path.clone();
                    let secondary_click_path = path.clone();
                    let aux_click_path = path.clone();
                    view.on_click_stop({
                        let kind = kind.clone();
                        move |_| {
                            click_data.click(&click_path, config);
                            select.update(|x| *x = Some(kind.clone()));
                        }
                    })
                    .on_double_click({
                        move |_| {
                            double_click_data
                                .double_click(&double_click_path, config)
                        }
                    })
                    .on_secondary_click_stop(move |_| {
                        secondary_click_data.secondary_click(&secondary_click_path);
                    })
                    .on_event_stop(
                        EventListener::PointerDown,
                        move |event| {
                            if let Event::PointerDown(pointer_event) = event {
                                if pointer_event.button.is_auxiliary() {
                                    aux_click_data.middle_click(&aux_click_path);
                                }
                            }
                        },
                    )
                } else {
                    view
                }
            },
        )
        .item_size_fixed(move || ui_line_height.get())
        .style(|s| s.absolute().flex_col().min_width_full()),
    )
    .style(|s| {
        s.absolute()
            .size_full()
            .line_height(LapceLayout::UI_LINE_HEIGHT as f32)
    })
    .on_secondary_click_stop(move |_| {
        if let Naming::None = naming.get_untracked() {
            if let Some(path) = &secondary_click_data.common.workspace.path {
                secondary_click_data.secondary_click(path);
            }
        }
    })
    .on_resize(move |rect| {
        scroll_rect.set(rect);
    })
    .scroll_to(move || {
        if let Some(line) = scroll_to_line.get() {
            let line_height = ui_line_height.get_untracked();
            Some(
                (
                    0.0,
                    line * line_height - scroll_rect.get_untracked().height() / 2.0,
                )
                    .into(),
            )
        } else {
            None
        }
    })
    .scroll_style(|s| s.hide_bars(true))
}

fn open_editors_view(workspace_data: Rc<WorkspaceData>) -> impl View {
    let editors = workspace_data.main_split.editors;
    let editor_tabs = workspace_data.main_split.editor_tabs;
    let config = workspace_data.common.config;
    let internal_command = workspace_data.common.internal_command;
    let active_editor_tab = workspace_data.main_split.active_editor_tab;
    let child_view = move |editor_tab: RwSignal<EditorTabData>,
                           child_index: RwSignal<usize>,
                           child: EditorTabChild| {
        let editor_tab_id =
            editor_tab.with_untracked(|editor_tab| editor_tab.editor_tab_id);
        let child_for_close = child.clone();
        let info = child.view_info(editors, config, None);
        let hovered = create_rw_signal(false);

        stack((
            clickable_icon(
                move || {
                    if hovered.get() || info.with(|info| info.is_pristine) {
                        LapceIcons::CLOSE
                    } else {
                        LapceIcons::UNSAVED
                    }
                },
                move || {
                    let editor_tab_id =
                        editor_tab.with_untracked(|t| t.editor_tab_id);
                    internal_command.send(InternalCommand::EditorTabChildClose {
                        editor_tab_id,
                        child: child_for_close.clone(),
                    });
                },
                || false,
                || false,
                || "Close",
                config,
            )
            .on_event_stop(EventListener::PointerEnter, move |_| {
                hovered.set(true);
            })
            .on_event_stop(EventListener::PointerLeave, move |_| {
                hovered.set(false);
            })
            .on_event_stop(EventListener::PointerDown, |_| {})
            .style(|s| s.margin_left(10.0)),
            container(svg(move || info.with(|info| info.icon.clone())).style(
                move |s| {
                    let size = config.get().ui.icon_size() as f32;
                    s.size(size, size)
                        .apply_opt(info.with(|info| info.color), |s, c| s.color(c))
                },
            ))
            .style(|s| s.padding_horiz(6.0)),
            label(move || info.with(|info| info.name.clone())),
        ))
        .style(move |s| {
            let config = config.get();
            s.items_center()
                .width_pct(100.0)
                .cursor(CursorStyle::Pointer)
                .apply_if(
                    active_editor_tab.get() == Some(editor_tab_id)
                        && editor_tab.with(|editor_tab| editor_tab.active)
                            == child_index.get(),
                    |s| {
                        s.background(
                            config.color(LapceColor::PANEL_CURRENT_BACKGROUND),
                        )
                    },
                )
                .hover(|s| {
                    s.background(config.color(LapceColor::PANEL_HOVERED_BACKGROUND))
                })
        })
        .on_event_cont(EventListener::PointerDown, move |_| {
            editor_tab.update(|editor_tab| {
                editor_tab.active = child_index.get_untracked();
            });
            active_editor_tab.set(Some(editor_tab_id));
        })
    };

    scroll(
        dyn_stack(
            move || editor_tabs.get().into_iter().enumerate(),
            move |(index, (editor_tab_id, _))| (*index, *editor_tab_id),
            move |(index, (_, editor_tab))| {
                stack((
                    label(move || format!("Group {}", index + 1))
                        .style(|s| s.margin_left(10.0)),
                    dyn_stack(
                        move || editor_tab.get().children,
                        move |(_, _, child)| child.id(),
                        move |(child_index, _, child)| {
                            child_view(editor_tab, child_index, child)
                        },
                    )
                    .style(|s| s.flex_col().width_pct(100.0)),
                ))
                .style(|s| s.flex_col())
            },
        )
        .style(|s| s.flex_col().width_pct(100.0)),
    )
    .style(|s| {
        s.absolute()
            .size_full()
            .line_height(LapceLayout::UI_LINE_HEIGHT as f32)
    })
    .debug_name("Open Editors")
}
