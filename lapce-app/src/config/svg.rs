use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use include_dir::{Dir, include_dir};

use crate::config::LOGO;

// Three icon directories embedded at compile time via include_dir!.
// This means the app carries all default icons without needing external files.
const CODICONS_ICONS_DIR: Dir =
    include_dir!("$CARGO_MANIFEST_DIR/../icons/codicons");
const LAPCE_ICONS_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../icons/lapce");
const FILETYPES_ICONS_DIR: Dir =
    include_dir!("$CARGO_MANIFEST_DIR/../icons/filetypes");

/// Caches SVG content strings to avoid re-reading embedded or on-disk files.
/// Two separate caches: `svgs` for embedded defaults (keyed by name), and
/// `svgs_on_disk` for plugin theme icons (keyed by absolute path, with None
/// for files that failed to read, so we don't retry).
#[derive(Debug, Clone)]
pub struct SvgStore {
    svgs: HashMap<String, String>,
    svgs_on_disk: HashMap<PathBuf, Option<String>>,
}

impl Default for SvgStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SvgStore {
    fn new() -> Self {
        let mut svgs = HashMap::new();
        svgs.insert("lapce_logo".to_string(), LOGO.to_string());

        Self {
            svgs,
            svgs_on_disk: HashMap::new(),
        }
    }

    pub fn logo_svg(&self) -> String {
        self.svgs.get("lapce_logo").unwrap().clone()
    }

    pub fn get_default_svg(&mut self, name: &str) -> Option<String> {
        if !self.svgs.contains_key(name) {
            let file = if name == "lapce_logo.svg" {
                LAPCE_ICONS_DIR.get_file(name)
            } else {
                CODICONS_ICONS_DIR
                    .get_file(name)
                    .or_else(|| FILETYPES_ICONS_DIR.get_file(name))
            };
            let Some(file) = file else {
                tracing::warn!("Failed to find embedded SVG: {name}");
                return None;
            };
            let Some(content) = file.contents_utf8() else {
                tracing::warn!("Embedded SVG is not valid UTF-8: {name}");
                return None;
            };
            self.svgs.insert(name.to_string(), content.to_string());
        }
        self.svgs.get(name).cloned()
    }

    pub fn get_svg_on_disk(&mut self, path: &Path) -> Option<String> {
        if !self.svgs_on_disk.contains_key(path) {
            let svg = fs::read_to_string(path).ok();
            self.svgs_on_disk.insert(path.to_path_buf(), svg);
        }

        self.svgs_on_disk.get(path).unwrap().clone()
    }
}
