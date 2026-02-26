use std::path::Path;

use lapce_rpc::project::{ProjectInfo, ProjectKind};

/// Find the project root for a given file by walking UP the directory tree.
///
/// For each ancestor directory (starting from the file's parent), checks for
/// well-known marker files. Returns the deepest match (closest ancestor with
/// a marker), which handles nested projects correctly (e.g., a Rust workspace
/// containing a sub-crate).
///
/// This is O(depth × marker_count) — typically ~7 directories × ~12 markers
/// = ~84 stat calls, completing in <1ms.
pub fn find_project_for_file(
    file_path: &Path,
    workspace: Option<&Path>,
) -> Option<ProjectInfo> {
    let start = if file_path.is_file() {
        file_path.parent()?
    } else {
        file_path
    };

    // Walk up from the file's directory to the workspace root (or filesystem root).
    let mut current = start;
    loop {
        for kind in ProjectKind::all() {
            for marker in kind.marker_files() {
                if current.join(marker).exists() {
                    let languages: Vec<String> =
                        kind.lsp_languages().iter().map(|s| s.to_string()).collect();

                    tracing::info!(
                        "[project] Found {:?} project at {:?} (marker: {}, from file: {:?})",
                        kind,
                        current,
                        marker,
                        file_path,
                    );

                    return Some(ProjectInfo {
                        root: current.to_path_buf(),
                        kind: kind.clone(),
                        languages,
                        marker_file: marker.to_string(),
                        tool_versions: Vec::new(),
                        version_manager: None,
                        lsp_servers: Vec::new(),
                    });
                }
            }
        }

        // Stop at workspace root — don't walk above it.
        if let Some(ws) = workspace {
            if current == ws {
                break;
            }
        }

        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }

    None
}
