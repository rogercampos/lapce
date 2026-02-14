use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{main_split::SplitInfo, panel::data::PanelInfo};

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
}
