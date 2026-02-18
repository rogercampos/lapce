use std::{path::PathBuf, sync::Arc};

use floem::{
    View,
    reactive::{ReadSignal, RwSignal, SignalGet, SignalWith},
    style::Display,
    views::{Decorators, label, scroll, stack, text},
};
use lapce_rpc::project::ProjectInfo;

use crate::config::{LapceConfig, color::LapceColor};

/// A small pill-shaped badge for inline metadata display.
fn badge(text_str: String, config: ReadSignal<Arc<LapceConfig>>) -> impl View {
    label(move || text_str.clone()).style(move |s| {
        let config = config.get();
        s.padding_horiz(6.0)
            .padding_vert(1.0)
            .border_radius(3.0)
            .font_size((config.ui.font_size() as f32 * 0.85).max(9.0))
            .background(
                config
                    .color(LapceColor::EDITOR_FOREGROUND)
                    .multiply_alpha(0.08),
            )
            .color(config.color(LapceColor::EDITOR_FOREGROUND))
    })
}

fn project_card(
    project: ProjectInfo,
    workspace_path: Option<PathBuf>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let relative_path = workspace_path
        .as_deref()
        .and_then(|ws| project.root.strip_prefix(ws).ok())
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| project.root.to_string_lossy().to_string());

    let kind_label = project.kind.label().to_string();

    let mut info_parts: Vec<String> = Vec::new();
    if let Some(vm) = &project.version_manager {
        info_parts.push(vm.clone());
    }
    for (tool, version) in &project.tool_versions {
        if !version.starts_with('/') && !version.starts_with('~') {
            info_parts.push(format!("{tool} {version}"));
        }
    }
    let info_text = info_parts.join(" · ");

    stack((
        // Left: path + badge
        stack((
            label(move || relative_path.clone()).style(move |s| {
                s.font_bold()
                    .color(config.get().color(LapceColor::EDITOR_FOREGROUND))
                    .text_ellipsis()
                    .min_width(0)
            }),
            badge(kind_label, config),
        ))
        .style(|s| {
            s.flex_row()
                .items_center()
                .gap(8.0)
                .flex_grow(1.0)
                .min_width(0)
        }),
        // Right: compact tool info
        label(move || info_text.clone()).style(move |s| {
            let config = config.get();
            s.color(config.color(LapceColor::EDITOR_DIM))
                .font_size((config.ui.font_size() as f32 * 0.85).max(9.0))
                .flex_shrink(0.0)
        }),
    ))
    .style(move |s| {
        let config = config.get();
        s.flex_row()
            .items_center()
            .width_full()
            .padding_horiz(12.0)
            .padding_vert(8.0)
            .gap(12.0)
            .border_bottom(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
    })
}

pub fn projects_view(
    projects: RwSignal<Vec<ProjectInfo>>,
    workspace_path: Option<PathBuf>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    stack((
        // Header
        stack((
            label(|| "Projects".to_string()).style(move |s| {
                let config = config.get();
                s.font_bold()
                    .font_size(config.ui.font_size() as f32 + 2.0)
                    .color(config.color(LapceColor::EDITOR_FOREGROUND))
            }),
            label(move || {
                let count = projects.with(|p| p.len());
                if count == 0 {
                    String::new()
                } else {
                    format!("({count})")
                }
            })
            .style(move |s| {
                s.margin_left(8.0)
                    .color(config.get().color(LapceColor::EDITOR_DIM))
            }),
        ))
        .style(move |s| {
            s.flex_row()
                .items_center()
                .padding(12.0)
                .width_full()
                .border_bottom(1.0)
                .border_color(config.get().color(LapceColor::LAPCE_BORDER))
        }),
        // Scrollable project list
        scroll({
            let ws = workspace_path.clone();
            stack((
                label(move || {
                    // Force re-render when projects change
                    let _ = projects.get();
                    String::new()
                })
                .style(|s| s.hide()),
                // We use dyn_stack instead of virtual_stack to avoid the
                // scroll sizing issues that plagued the popup approach
                floem::views::dyn_stack(
                    move || {
                        projects.get().into_iter().enumerate().collect::<Vec<_>>()
                    },
                    |(i, p)| {
                        format!("{}:{}:{}", i, p.root.display(), p.kind.label())
                    },
                    {
                        let ws = ws.clone();
                        move |(_i, project)| {
                            project_card(project, ws.clone(), config)
                        }
                    },
                )
                .style(|s| s.flex_col().width_full()),
            ))
            .style(|s| s.flex_col().width_full())
        })
        .style(|s| s.width_full().flex_grow(1.0).flex_basis(0.0)),
        // Empty state
        text("No projects detected in this workspace").style(move |s| {
            s.display(if projects.with(|p| p.is_empty()) {
                Display::Flex
            } else {
                Display::None
            })
            .padding(20.0)
            .color(config.get().color(LapceColor::EDITOR_DIM))
        }),
    ))
    .style(move |s| {
        s.flex_col()
            .size_full()
            .background(config.get().color(LapceColor::EDITOR_BACKGROUND))
    })
    .debug_name("Projects View")
}
