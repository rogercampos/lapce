use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    main_split::SplitInfo, panel::data::PanelInfo, search_tabs::SearchTabInfo,
};

/// The type of workspace connection. Currently only Local is supported.
/// Remote workspace support was previously available but has been removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LapceWorkspaceType {
    Local,
}

impl std::fmt::Display for LapceWorkspaceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LapceWorkspaceType::Local => f.write_str("Local"),
        }
    }
}

/// Identifies a workspace (project folder). `path` is None for empty/bare windows.
/// `last_open` is a Unix timestamp used for sorting the recent workspaces list.
/// The Display impl produces "Local:/path/to/project" which is used as the persistence key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LapceWorkspace {
    pub kind: LapceWorkspaceType,
    pub path: Option<PathBuf>,
    pub last_open: u64,
}

impl LapceWorkspace {
    pub fn display(&self) -> Option<String> {
        let path = self.path.as_ref()?;
        let path = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .to_string();
        Some(path)
    }
}

impl Default for LapceWorkspace {
    fn default() -> Self {
        Self {
            kind: LapceWorkspaceType::Local,
            path: None,
            last_open: 0,
        }
    }
}

impl std::fmt::Display for LapceWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            self.kind,
            self.path.as_ref().and_then(|p| p.to_str()).unwrap_or("")
        )
    }
}

/// The complete serializable state of a workspace, persisted to disk.
/// Contains the recursive split tree layout and panel configuration.
/// This is what gets saved to db/workspaces/<id>/workspace_info and
/// restored on next launch to recreate the exact editor layout.
#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub split: SplitInfo,
    pub panel: PanelInfo,
    #[serde(default)]
    pub search_tabs: Vec<SearchTabInfo>,
    #[serde(default)]
    pub active_search_tab: usize,
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    #[serde(default)]
    pub starred_folders: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_workspace_is_local_with_no_path() {
        let ws = LapceWorkspace::default();
        assert_eq!(ws.kind, LapceWorkspaceType::Local);
        assert_eq!(ws.path, None);
        assert_eq!(ws.last_open, 0);
    }

    #[test]
    fn display_returns_none_when_no_path() {
        let ws = LapceWorkspace::default();
        assert_eq!(ws.display(), None);
    }

    #[test]
    fn display_returns_file_name_for_normal_path() {
        let ws = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/home/user/my-project")),
            last_open: 0,
        };
        assert_eq!(ws.display(), Some("my-project".to_string()));
    }

    #[test]
    fn display_returns_full_path_for_root() {
        let ws = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/")),
            last_open: 0,
        };
        // "/" has no file_name, so it falls back to the full OsStr
        assert_eq!(ws.display(), Some("/".to_string()));
    }

    #[test]
    fn display_trait_with_path() {
        let ws = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/home/user/project")),
            last_open: 0,
        };
        assert_eq!(format!("{ws}"), "Local:/home/user/project");
    }

    #[test]
    fn display_trait_without_path() {
        let ws = LapceWorkspace::default();
        assert_eq!(format!("{ws}"), "Local:");
    }

    #[test]
    fn workspace_type_display() {
        assert_eq!(format!("{}", LapceWorkspaceType::Local), "Local");
    }

    #[test]
    fn serialization_roundtrip() {
        let ws = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/home/user/project")),
            last_open: 1234567890,
        };
        let json = serde_json::to_string(&ws).unwrap();
        let deserialized: LapceWorkspace = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.kind, ws.kind);
        assert_eq!(deserialized.path, ws.path);
        assert_eq!(deserialized.last_open, ws.last_open);
    }

    #[test]
    fn serialization_roundtrip_no_path() {
        let ws = LapceWorkspace::default();
        let json = serde_json::to_string(&ws).unwrap();
        let deserialized: LapceWorkspace = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.kind, ws.kind);
        assert_eq!(deserialized.path, ws.path);
        assert_eq!(deserialized.last_open, ws.last_open);
    }
}
