use std::{rc::Rc, sync::Arc};

use floem::{
    AnyView, IntoView, View,
    event::{Event, EventListener},
    kurbo::{Point, Size},
    reactive::{
        ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith, create_rw_signal,
    },
    style::{CursorStyle, Style},
    taffy::AlignItems,
    unit::PxPctAuto,
    views::{
        Decorators, container, dyn_stack, empty, h_stack, label, stack,
        stack_from_iter, tab, text,
    },
};

use super::{
    global_search_view::global_search_panel,
    kind::PanelKind,
    position::{PanelContainerPosition, PanelPosition},
};
use crate::{
    app::{clickable_icon, clickable_icon_base},
    config::{LapceConfig, color::LapceColor, icon::LapceIcons},
    file_explorer::view::file_explorer_panel,
    workspace_data::WorkspaceData,
};

/// Creates a foldable section with a clickable header that toggles child visibility.
/// The fold icon (chevron) rotates to indicate state. The child content is hidden via
/// style rather than removed from the tree, so state is preserved across folds.
pub fn foldable_panel_section(
    header: impl View + 'static,
    child: impl View + 'static,
    open: RwSignal<bool>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    stack((
        h_stack((
            clickable_icon_base(
                move || {
                    if open.get() {
                        LapceIcons::PANEL_FOLD_DOWN
                    } else {
                        LapceIcons::PANEL_FOLD_UP
                    }
                },
                None::<Box<dyn Fn()>>,
                || false,
                || false,
                config,
            ),
            header.style(|s| s.align_items(AlignItems::Center).padding_left(3.0)),
        ))
        .style(move |s| {
            s.padding_horiz(10.0)
                .padding_vert(6.0)
                .width_pct(100.0)
                .cursor(CursorStyle::Pointer)
                .background(config.get().color(LapceColor::EDITOR_BACKGROUND))
        })
        .on_click_stop(move |_| {
            open.update(|open| *open = !*open);
        }),
        child.style(move |s| s.apply_if(!open.get(), |s| s.hide())),
    ))
}

/// A builder for creating a foldable panel out of sections
pub struct PanelBuilder {
    views: Vec<AnyView>,
    config: ReadSignal<Arc<LapceConfig>>,
    position: PanelPosition,
}
impl PanelBuilder {
    pub fn new(
        config: ReadSignal<Arc<LapceConfig>>,
        position: PanelPosition,
    ) -> Self {
        Self {
            views: Vec::new(),
            config,
            position,
        }
    }

    /// Core method that wraps content in a foldable section and applies sizing rules.
    /// When open: uses explicit height if provided, otherwise flex-grows to fill space.
    /// When closed on bottom panels: still takes some flex space (0.3) so the header
    /// remains visible alongside its peer section. On side panels, collapsed sections
    /// shrink to just the header height (no explicit size set).
    fn add_general_with_header(
        mut self,
        header: impl View + 'static,
        height: Option<PxPctAuto>,
        view: impl View + 'static,
        open: RwSignal<bool>,
        style: impl Fn(Style) -> Style + 'static,
    ) -> Self {
        let position = self.position;
        let view = foldable_panel_section(header, view, open, self.config).style(
            move |s| {
                let s = s.width_full().flex_col();
                // Use the manual height if given, otherwise if we're open behave flex,
                // otherwise, do nothing so that there's no height
                let s = if open.get() {
                    if let Some(height) = height {
                        s.height(height)
                    } else {
                        s.flex_grow(1.0).flex_basis(0.0)
                    }
                } else if position.is_bottom() {
                    s.flex_grow(0.3).flex_basis(0.0)
                } else {
                    s
                };

                style(s)
            },
        );
        self.views.push(view.into_any());
        self
    }

    fn add_general(
        self,
        name: &'static str,
        height: Option<PxPctAuto>,
        view: impl View + 'static,
        open: RwSignal<bool>,
        style: impl Fn(Style) -> Style + 'static,
    ) -> Self {
        self.add_general_with_header(
            text(name).style(move |s| s.selectable(false)),
            height,
            view,
            open,
            style,
        )
    }

    /// Add a view to the panel
    pub fn add(
        self,
        name: &'static str,
        view: impl View + 'static,
        open: RwSignal<bool>,
    ) -> Self {
        self.add_general(name, None, view, open, std::convert::identity)
    }

    /// Add a view to the panel with a custom header view
    pub fn add_with_header(
        self,
        header: impl View + 'static,
        view: impl View + 'static,
        open: RwSignal<bool>,
    ) -> Self {
        self.add_general_with_header(
            header,
            None,
            view,
            open,
            std::convert::identity,
        )
    }

    /// Add a view to the panel with a custom style applied to the overall header+section-content
    pub fn add_style(
        self,
        name: &'static str,
        view: impl View + 'static,
        open: RwSignal<bool>,
        style: impl Fn(Style) -> Style + 'static,
    ) -> Self {
        self.add_general(name, None, view, open, style)
    }

    /// Add a view to the panel with a custom height that is only used when the panel is open
    pub fn add_height(
        self,
        name: &'static str,
        height: impl Into<PxPctAuto>,
        view: impl View + 'static,
        open: RwSignal<bool>,
    ) -> Self {
        self.add_general(
            name,
            Some(height.into()),
            view,
            open,
            std::convert::identity,
        )
    }

    /// Add a view to the panel with a custom height that is only used when the panel is open
    /// and a custom style applied to the overall header+section-content
    pub fn add_height_style(
        self,
        name: &'static str,
        height: impl Into<PxPctAuto>,
        view: impl View + 'static,
        open: RwSignal<bool>,
        style: impl Fn(Style) -> Style + 'static,
    ) -> Self {
        self.add_general(name, Some(height.into()), view, open, style)
    }

    /// Add a view to the panel with a custom height that is only used when the panel is open
    pub fn add_height_pct(
        self,
        name: &'static str,
        height: f64,
        view: impl View + 'static,
        open: RwSignal<bool>,
    ) -> Self {
        self.add_general(
            name,
            Some(PxPctAuto::Pct(height)),
            view,
            open,
            std::convert::identity,
        )
    }

    /// Build the panel into a view. Bottom panels lay out sections horizontally
    /// (side by side), while side panels lay out sections vertically (stacked).
    pub fn build(self) -> impl View {
        stack_from_iter(self.views).style(move |s| {
            s.width_full()
                .apply_if(!self.position.is_bottom(), |s| s.flex_col())
        })
    }
}

/// Builds the full container view for one of the three panel areas (Left, Bottom, Right).
/// Contains: two panel pickers (icon strips for tab selection), two panel content views
/// (one per position half), and a drag handle for resizing. The container auto-hides
/// when neither position half is shown.
pub fn panel_container_view(
    workspace_data: Rc<WorkspaceData>,
    position: PanelContainerPosition,
) -> impl View {
    let panel = workspace_data.panel.clone();
    let config = workspace_data.common.config;
    let current_size = create_rw_signal(Size::ZERO);
    let available_size = workspace_data.panel.available_size;

    // The resize drag view is an invisible 4px-wide/tall strip placed on the edge
    // of the panel container. On drag, it updates panel size while clamping to
    // min/max bounds. For the bottom panel, dragging past a threshold toggles maximize.
    let resize_drag_view = {
        let panel = panel.clone();
        let panel_size = panel.size;
        move |position: PanelContainerPosition| {
            panel.panel_info();
            let view = empty();
            let view_id = view.id();
            let drag_start: RwSignal<Option<Point>> = create_rw_signal(None);
            view.on_event_stop(EventListener::PointerDown, move |event| {
                view_id.request_active();
                if let Event::PointerDown(pointer_event) = event {
                    drag_start.set(Some(pointer_event.pos));
                }
            })
            .on_event_stop(EventListener::PointerMove, move |event| {
                if let Event::PointerMove(pointer_event) = event {
                    if let Some(drag_start_point) = drag_start.get_untracked() {
                        let current_size = current_size.get_untracked();
                        let available_size = available_size.get_untracked();
                        match position {
                            PanelContainerPosition::Left => {
                                let new_size = current_size.width
                                    + pointer_event.pos.x
                                    - drag_start_point.x;
                                let current_panel_size = panel_size.get_untracked();
                                let new_size = new_size
                                    .max(150.0)
                                    .min(available_size.width - 150.0 - 150.0);
                                if new_size != current_panel_size.left {
                                    panel_size.update(|size| {
                                        size.left = new_size;
                                        size.right = size.right.min(
                                            available_size.width - new_size - 150.0,
                                        )
                                    })
                                }
                            }
                            PanelContainerPosition::Bottom => {
                                let new_size = current_size.height
                                    - (pointer_event.pos.y - drag_start_point.y);
                                let maximized = panel.panel_bottom_maximized(false);
                                if (maximized
                                    && new_size < available_size.height - 50.0)
                                    || (!maximized
                                        && new_size > available_size.height - 50.0)
                                {
                                    panel.toggle_bottom_maximize();
                                }

                                let new_size = new_size
                                    .max(100.0)
                                    .min(available_size.height - 100.0);
                                let current_size =
                                    panel_size.with_untracked(|s| s.bottom);
                                if current_size != new_size {
                                    panel_size.update(|size| {
                                        size.bottom = new_size;
                                    })
                                }
                            }
                            PanelContainerPosition::Right => {
                                let new_size = current_size.width
                                    - (pointer_event.pos.x - drag_start_point.x);
                                let current_panel_size = panel_size.get_untracked();
                                let new_size = new_size
                                    .max(150.0)
                                    .min(available_size.width - 150.0 - 150.0);
                                if new_size != current_panel_size.right {
                                    panel_size.update(|size| {
                                        size.right = new_size;
                                        size.left = size.left.min(
                                            available_size.width - new_size - 150.0,
                                        )
                                    })
                                }
                            }
                        }
                    }
                }
            })
            .on_event_stop(EventListener::PointerUp, move |_| {
                drag_start.set(None);
            })
            .style(move |s| {
                let is_dragging = drag_start.get().is_some();
                let current_size = current_size.get();
                let config = config.get();
                s.absolute()
                    .apply_if(position == PanelContainerPosition::Bottom, |s| {
                        s.width_pct(100.0).height(4.0).margin_top(-2.0)
                    })
                    .apply_if(position == PanelContainerPosition::Left, |s| {
                        s.width(4.0)
                            .margin_left(current_size.width as f32 - 2.0)
                            .height_pct(100.0)
                    })
                    .apply_if(position == PanelContainerPosition::Right, |s| {
                        s.width(4.0).margin_left(-2.0).height_pct(100.0)
                    })
                    .apply_if(is_dragging, |s| {
                        s.background(config.color(LapceColor::EDITOR_CARET))
                            .apply_if(
                                position == PanelContainerPosition::Bottom,
                                |s| s.cursor(CursorStyle::RowResize),
                            )
                            .apply_if(
                                position != PanelContainerPosition::Bottom,
                                |s| s.cursor(CursorStyle::ColResize),
                            )
                            .z_index(2)
                    })
                    .hover(|s| {
                        s.background(config.color(LapceColor::EDITOR_CARET))
                            .apply_if(
                                position == PanelContainerPosition::Bottom,
                                |s| s.cursor(CursorStyle::RowResize),
                            )
                            .apply_if(
                                position != PanelContainerPosition::Bottom,
                                |s| s.cursor(CursorStyle::ColResize),
                            )
                            .z_index(2)
                    })
            })
        }
    };

    let is_bottom = position.is_bottom();
    stack((
        panel_picker(workspace_data.clone(), position.first()),
        panel_view(workspace_data.clone(), position.first()),
        panel_view(workspace_data.clone(), position.second()),
        panel_picker(workspace_data.clone(), position.second()),
        resize_drag_view(position),
    ))
    .on_resize(move |rect| {
        let size = rect.size();
        if size != current_size.get_untracked() {
            current_size.set(size);
        }
    })
    .style(move |s| {
        let size = panel.size.with(|s| match position {
            PanelContainerPosition::Left => s.left,
            PanelContainerPosition::Bottom => s.bottom,
            PanelContainerPosition::Right => s.right,
        });
        let is_maximized = panel.panel_bottom_maximized(true);
        let config = config.get();
        s.apply_if(!panel.is_container_shown(&position, true), |s| s.hide())
            .apply_if(position == PanelContainerPosition::Bottom, |s| {
                s.width_pct(100.0)
                    .apply_if(!is_maximized, |s| {
                        s.border_top(1.0).height(size as f32)
                    })
                    .apply_if(is_maximized, |s| s.flex_grow(1.0))
            })
            .apply_if(position == PanelContainerPosition::Left, |s| {
                s.border_right(1.0)
                    .width(size as f32)
                    .height_pct(100.0)
                    .background(config.color(LapceColor::PANEL_BACKGROUND))
            })
            .apply_if(position == PanelContainerPosition::Right, |s| {
                s.border_left(1.0)
                    .width(size as f32)
                    .height_pct(100.0)
                    .background(config.color(LapceColor::PANEL_BACKGROUND))
            })
            .apply_if(!is_bottom, |s| s.flex_col())
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .color(config.color(LapceColor::PANEL_FOREGROUND))
    })
    .debug_name(format!("{:?} Pannel Container View", position))
}

/// Renders the active panel content for a given position using a `tab` view that
/// switches between panel kinds. The tab view is reactive: when `active` changes
/// in the style, the displayed panel kind switches without recreating the view tree.
/// Hidden when the position is not shown or has no panels assigned.
fn panel_view(
    workspace_data: Rc<WorkspaceData>,
    position: PanelPosition,
) -> impl View {
    let panel = workspace_data.panel.clone();
    let panels = move || {
        panel
            .panels
            .with(|p| p.get(&position).cloned().unwrap_or_default())
    };
    let active_fn = move || {
        panel
            .styles
            .with(|s| s.get(&position).map(|s| s.active).unwrap_or(0))
    };
    tab(
        active_fn,
        panels,
        |p| *p,
        move |kind| {
            let view = match kind {
                PanelKind::FileExplorer => {
                    file_explorer_panel(workspace_data.clone(), position).into_any()
                }
                PanelKind::Search => {
                    global_search_panel(workspace_data.clone(), position).into_any()
                }
            };
            view.style(|s| s.size_pct(100.0, 100.0))
        },
    )
    .style(move |s| {
        s.size_pct(100.0, 100.0).apply_if(
            !panel.is_position_shown(&position, true)
                || panel.is_position_empty(&position, true),
            |s| s.hide(),
        )
    })
}

pub fn panel_header(
    header: String,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    container(label(move || header.clone())).style(move |s| {
        s.padding_horiz(10.0)
            .padding_vert(6.0)
            .width_pct(100.0)
            .background(config.get().color(LapceColor::EDITOR_BACKGROUND))
    })
}

/// Renders the icon strip for switching between panel kinds at a given position.
/// Each icon shows the panel's SVG and has an active indicator (a colored border line).
/// The picker is hidden when only one or zero panels exist at this position (no
/// need to switch). Clicking an icon toggles that panel's visibility.
fn panel_picker(
    workspace_data: Rc<WorkspaceData>,
    position: PanelPosition,
) -> impl View {
    let panel = workspace_data.panel.clone();
    let panels = panel.panels;
    let config = workspace_data.common.config;
    let is_bottom = position.is_bottom();
    let is_first = position.is_first();
    dyn_stack(
        move || {
            panel
                .panels
                .with(|panels| panels.get(&position).cloned().unwrap_or_default())
        },
        |p| *p,
        move |p| {
            let workspace_data = workspace_data.clone();
            let tooltip = match p {
                PanelKind::FileExplorer => "File Explorer",
                PanelKind::Search => "Search",
            };
            let icon = p.svg_name();
            let is_active = {
                let workspace_data = workspace_data.clone();
                move || {
                    if let Some((active_panel, shown)) = workspace_data
                        .panel
                        .active_panel_at_position(&position, true)
                    {
                        shown && active_panel == p
                    } else {
                        false
                    }
                }
            };
            container(stack((
                clickable_icon(
                    || icon,
                    move || {
                        workspace_data.toggle_panel_visual(p);
                    },
                    || false,
                    || false,
                    move || tooltip,
                    config,
                )
                .style(|s| s.padding(1.0)),
                label(|| "".to_string()).style(move |s| {
                    s.selectable(false)
                        .pointer_events_none()
                        .absolute()
                        .size_pct(100.0, 100.0)
                        .apply_if(!is_bottom && is_first, |s| s.margin_top(2.0))
                        .apply_if(!is_bottom && !is_first, |s| s.margin_top(-2.0))
                        .apply_if(is_bottom && is_first, |s| s.margin_left(-2.0))
                        .apply_if(is_bottom && !is_first, |s| s.margin_left(2.0))
                        .apply_if(is_active(), |s| {
                            s.apply_if(!is_bottom && is_first, |s| {
                                s.border_bottom(2.0)
                            })
                            .apply_if(!is_bottom && !is_first, |s| s.border_top(2.0))
                            .apply_if(is_bottom && is_first, |s| s.border_left(2.0))
                            .apply_if(is_bottom && !is_first, |s| {
                                s.border_right(2.0)
                            })
                        })
                        .border_color(
                            config
                                .get()
                                .color(LapceColor::LAPCE_TAB_ACTIVE_UNDERLINE),
                        )
                }),
            )))
            .style(|s| s.padding(6.0))
        },
    )
    .style(move |s| {
        s.border_color(config.get().color(LapceColor::LAPCE_BORDER))
            .apply_if(
                panels.with(|p| {
                    p.get(&position).map(|p| p.len() <= 1).unwrap_or(true)
                }),
                |s| s.hide(),
            )
            .apply_if(is_bottom, |s| s.flex_col())
            .apply_if(is_bottom && is_first, |s| s.border_right(1.0))
            .apply_if(is_bottom && !is_first, |s| s.border_left(1.0))
            .apply_if(!is_bottom && is_first, |s| s.border_bottom(1.0))
            .apply_if(!is_bottom && !is_first, |s| s.border_top(1.0))
    })
}
