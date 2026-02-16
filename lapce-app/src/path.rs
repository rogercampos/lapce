use std::path::Path;

use lapce_core::directory::Directory;

pub fn display_path(path: &Path) -> String {
    if let Some(home) = Directory::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.to_string_lossy().to_string()
}
