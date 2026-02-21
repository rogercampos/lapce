use std::path::Path;

use lapce_rpc::project::{ProjectInfo, ProjectKind};

/// Detect sub-projects within a workspace by walking the directory tree
/// and looking for well-known marker files (Cargo.toml, Gemfile, etc.).
///
/// Uses `ignore::WalkBuilder` to respect `.gitignore` rules.
/// Max depth of 5 levels as a safety net against deeply nested repos.
pub fn detect_projects(workspace: &Path) -> Vec<ProjectInfo> {
    let mut projects = Vec::new();

    let walker = ignore::WalkBuilder::new(workspace)
        .max_depth(Some(5))
        .hidden(false)
        .parents(false)
        .require_git(false)
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let file_name = match entry.file_name().to_str() {
            Some(name) => name,
            None => continue,
        };

        // Log marker file candidates for debugging
        if ProjectKind::all()
            .iter()
            .any(|k| k.marker_files().contains(&file_name))
        {
            tracing::info!(
                "[detect] Walker found marker candidate: {:?}",
                entry.path()
            );
        }

        for kind in ProjectKind::all() {
            if kind.marker_files().contains(&file_name) {
                let root = match entry.path().parent() {
                    Some(parent) => parent.to_path_buf(),
                    None => continue,
                };

                let languages: Vec<String> =
                    kind.lsp_languages().iter().map(|s| s.to_string()).collect();

                tracing::info!(
                    "[detect] Found {:?} project at {:?} (marker: {})",
                    kind,
                    root,
                    file_name
                );
                projects.push(ProjectInfo {
                    root,
                    kind: kind.clone(),
                    languages,
                    marker_file: file_name.to_string(),
                    tool_versions: Vec::new(),
                    version_manager: None,
                    lsp_servers: Vec::new(),
                });
            }
        }
    }

    // Sort by path depth (shallowest first)
    projects.sort_by_key(|p| p.root.components().count());

    projects
}
