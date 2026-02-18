use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Sender, unbounded};
use floem::{peniko::kurbo::Vec2, reactive::SignalGet};
use lapce_core::directory::Directory;
use sha2::{Digest, Sha256};

use crate::{
    app::{AppData, AppInfo},
    config::layout::LapceLayout,
    doc::DocInfo,
    window::{WindowData, WindowInfo},
    workspace::{LapceWorkspace, WorkspaceInfo},
    workspace_data::WorkspaceData,
};

const APP: &str = "app";
const WINDOW: &str = "window";
const WORKSPACE_INFO: &str = "workspace_info";
const WORKSPACE_FILES: &str = "workspace_files";
const RECENT_WORKSPACES: &str = "recent_workspaces";

/// Events sent to the background save thread. All persistence operations are
/// asynchronous: callers send events through a channel and don't block on I/O.
/// This prevents file writes from causing UI jank.
pub enum SaveEvent {
    App(AppInfo),
    Workspace(LapceWorkspace, WorkspaceInfo),
    RecentWorkspace(LapceWorkspace),
    Doc(DocInfo),
}

/// File-based persistence layer. All data is stored as JSON files in the config
/// directory (e.g., ~/Library/Application Support/dev.lapce.Lapce-Debug/db/).
///
/// Structure:
/// - db/app: global app state (window positions/sizes)
/// - db/window: window layout info
/// - db/workspaces/<encoded-workspace>/workspace_info: split tree + panel layout
/// - db/workspaces/<encoded-workspace>/workspace_files/<sha256>: per-file cursor/scroll state
/// - db/disabled_volts: globally disabled plugins
/// - db/recent_workspaces: recently opened workspace list
///
/// Write operations are non-blocking: they're sent to a dedicated background thread
/// via crossbeam channel. Read operations are synchronous (called during startup).
#[derive(Clone)]
pub struct LapceDb {
    folder: PathBuf,
    workspace_folder: PathBuf,
    save_tx: Sender<SaveEvent>,
}

impl LapceDb {
    /// Creates a `LapceDb` backed by a custom folder, without spawning the
    /// background save thread. Only available in tests.
    #[cfg(test)]
    fn new_in(folder: PathBuf) -> Result<Self> {
        let workspace_folder = folder.join("workspaces");
        std::fs::create_dir_all(&workspace_folder)?;
        let (save_tx, _save_rx) = unbounded();
        Ok(Self {
            save_tx,
            workspace_folder,
            folder,
        })
    }

    /// Creates the db directory structure and spawns the background save thread.
    /// The save thread runs an infinite loop processing SaveEvents from the channel.
    /// Errors during individual saves are logged but don't crash the app.
    pub fn new() -> Result<Self> {
        let folder = Directory::config_directory()
            .ok_or_else(|| anyhow!("can't get config directory"))?
            .join("db");
        let workspace_folder = folder.join("workspaces");
        if let Err(err) = std::fs::create_dir_all(&workspace_folder) {
            tracing::error!("{:?}", err);
        }

        let (save_tx, save_rx) = unbounded();

        let db = Self {
            save_tx,
            workspace_folder,
            folder,
        };
        let local_db = db.clone();
        std::thread::Builder::new()
            .name("SaveEventHandler".to_owned())
            .spawn(move || -> Result<()> {
                loop {
                    let event = save_rx.recv()?;
                    match event {
                        SaveEvent::App(info) => {
                            if let Err(err) = local_db.insert_app_info(info) {
                                tracing::error!("{:?}", err);
                            }
                        }
                        SaveEvent::Workspace(workspace, info) => {
                            if let Err(err) =
                                local_db.insert_workspace(&workspace, &info)
                            {
                                tracing::error!("{:?}", err);
                            }
                        }
                        SaveEvent::RecentWorkspace(workspace) => {
                            if let Err(err) =
                                local_db.insert_recent_workspace(workspace)
                            {
                                tracing::error!("{:?}", err);
                            }
                        }
                        SaveEvent::Doc(info) => {
                            if let Err(err) = local_db.insert_doc(&info) {
                                tracing::error!("{:?}", err);
                            }
                        }
                    }
                }
            })
            .unwrap();
        Ok(db)
    }

    pub fn recent_workspaces(&self) -> Result<Vec<LapceWorkspace>> {
        let workspaces =
            std::fs::read_to_string(self.folder.join(RECENT_WORKSPACES))?;
        let workspaces: Vec<LapceWorkspace> = serde_json::from_str(&workspaces)?;
        Ok(workspaces)
    }

    pub fn update_recent_workspace(&self, workspace: &LapceWorkspace) -> Result<()> {
        if workspace.path.is_none() {
            return Ok(());
        }
        self.save_tx
            .send(SaveEvent::RecentWorkspace(workspace.clone()))?;
        Ok(())
    }

    /// Updates the recent workspaces list: either updates the timestamp of an existing
    /// entry or appends a new one. The list is sorted by most-recently-opened first.
    fn insert_recent_workspace(&self, workspace: LapceWorkspace) -> Result<()> {
        let mut workspaces = self.recent_workspaces().unwrap_or_default();

        let mut exists = false;
        for w in workspaces.iter_mut() {
            if w.path == workspace.path && w.kind == workspace.kind {
                w.last_open = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                exists = true;
                break;
            }
        }
        if !exists {
            let mut workspace = workspace;
            workspace.last_open = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            workspaces.push(workspace);
        }
        workspaces.sort_by_key(|w| -(w.last_open as i64));
        let workspaces = serde_json::to_string_pretty(&workspaces)?;
        std::fs::write(self.folder.join(RECENT_WORKSPACES), workspaces)?;

        Ok(())
    }

    pub fn save_workspace(&self, data: Rc<WorkspaceData>) -> Result<()> {
        let workspace = (*data.workspace).clone();
        let workspace_info = data.workspace_info();

        self.save_tx
            .send(SaveEvent::Workspace(workspace, workspace_info))?;

        Ok(())
    }

    pub fn get_workspace_info(
        &self,
        workspace: &LapceWorkspace,
    ) -> Result<WorkspaceInfo> {
        let info = std::fs::read_to_string(
            self.workspace_folder
                .join(workspace_folder_name(workspace))
                .join(WORKSPACE_INFO),
        )?;
        let info: WorkspaceInfo = serde_json::from_str(&info)?;
        Ok(info)
    }

    fn insert_workspace(
        &self,
        workspace: &LapceWorkspace,
        info: &WorkspaceInfo,
    ) -> Result<()> {
        let folder = self.workspace_folder.join(workspace_folder_name(workspace));
        if let Err(err) = std::fs::create_dir_all(&folder) {
            tracing::error!("{:?}", err);
        }
        let workspace_info = serde_json::to_string_pretty(info)?;
        std::fs::write(folder.join(WORKSPACE_INFO), workspace_info)?;
        Ok(())
    }

    pub fn save_app(&self, data: &AppData) -> Result<()> {
        let windows = data.windows.get_untracked();
        for (_, window) in &windows {
            if let Err(err) = self.save_window(window.clone()) {
                tracing::error!("{:?}", err);
            }
        }

        let info = AppInfo {
            windows: windows
                .iter()
                .map(|(_, window_data)| window_data.info())
                .collect(),
        };
        if info.windows.is_empty() {
            return Ok(());
        }

        self.save_tx.send(SaveEvent::App(info))?;

        Ok(())
    }

    pub fn insert_app_info(&self, info: AppInfo) -> Result<()> {
        let info = serde_json::to_string_pretty(&info)?;
        std::fs::write(self.folder.join(APP), info)?;
        Ok(())
    }

    pub fn insert_app(&self, data: AppData) -> Result<()> {
        let windows = data.windows.get_untracked();
        if windows.is_empty() {
            // insert_app is called after window is closed, so we don't want to store it
            return Ok(());
        }
        for (_, window) in &windows {
            if let Err(err) = self.insert_window(window.clone()) {
                tracing::error!("{:?}", err);
            }
        }
        let info = AppInfo {
            windows: windows
                .iter()
                .map(|(_, window_data)| window_data.info())
                .collect(),
        };
        self.insert_app_info(info)?;
        Ok(())
    }

    pub fn get_app(&self) -> Result<AppInfo> {
        let info = std::fs::read_to_string(self.folder.join(APP))?;
        let mut info: AppInfo = serde_json::from_str(&info)?;
        for window in info.windows.iter_mut() {
            if window.size.width < 10.0 {
                window.size.width = LapceLayout::DEFAULT_WINDOW_WIDTH;
            }
            if window.size.height < 10.0 {
                window.size.height = LapceLayout::DEFAULT_WINDOW_HEIGHT;
            }
        }
        Ok(info)
    }

    pub fn get_window(&self) -> Result<WindowInfo> {
        let info = std::fs::read_to_string(self.folder.join(WINDOW))?;
        let mut info: WindowInfo = serde_json::from_str(&info)?;
        if info.size.width < 10.0 {
            info.size.width = LapceLayout::DEFAULT_WINDOW_WIDTH;
        }
        if info.size.height < 10.0 {
            info.size.height = LapceLayout::DEFAULT_WINDOW_HEIGHT;
        }
        Ok(info)
    }

    pub fn save_window(&self, data: WindowData) -> Result<()> {
        if let Err(err) = self.save_workspace(data.workspace) {
            tracing::error!("{:?}", err);
        }
        Ok(())
    }

    pub fn insert_window(&self, data: WindowData) -> Result<()> {
        if let Err(err) = self.insert_workspace_data(data.workspace.clone()) {
            tracing::error!("{:?}", err);
        }
        let info = data.info();
        let info = serde_json::to_string_pretty(&info)?;
        std::fs::write(self.folder.join(WINDOW), info)?;
        Ok(())
    }

    pub fn insert_workspace_data(&self, data: Rc<WorkspaceData>) -> Result<()> {
        let workspace = (*data.workspace).clone();
        let workspace_info = data.workspace_info();

        self.insert_workspace(&workspace, &workspace_info)?;

        Ok(())
    }

    pub fn save_doc_position(
        &self,
        workspace: &LapceWorkspace,
        path: PathBuf,
        cursor_offset: usize,
        scroll_offset: Vec2,
    ) {
        let info = DocInfo {
            workspace: workspace.clone(),
            path,
            scroll_offset: (scroll_offset.x, scroll_offset.y),
            cursor_offset,
        };
        if let Err(err) = self.save_tx.send(SaveEvent::Doc(info)) {
            tracing::error!("{:?}", err);
        }
    }

    fn insert_doc(&self, info: &DocInfo) -> Result<()> {
        let folder = self
            .workspace_folder
            .join(workspace_folder_name(&info.workspace))
            .join(WORKSPACE_FILES);
        if let Err(err) = std::fs::create_dir_all(&folder) {
            tracing::error!("{:?}", err);
        }
        let contents = serde_json::to_string_pretty(info)?;
        std::fs::write(folder.join(doc_path_name(&info.path)), contents)?;
        Ok(())
    }

    pub fn get_doc_info(
        &self,
        workspace: &LapceWorkspace,
        path: &Path,
    ) -> Result<DocInfo> {
        let folder = self
            .workspace_folder
            .join(workspace_folder_name(workspace))
            .join(WORKSPACE_FILES);
        let info = std::fs::read_to_string(folder.join(doc_path_name(path)))?;
        let info: DocInfo = serde_json::from_str(&info)?;
        Ok(info)
    }
}

/// URL-encodes the workspace identifier (e.g., "Local:/path/to/project") to create
/// a filesystem-safe folder name for per-workspace persistence data.
fn workspace_folder_name(workspace: &LapceWorkspace) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_key_only(&workspace.to_string())
        .finish()
}

/// SHA-256 hashes the file path to generate a fixed-length, filesystem-safe filename
/// for per-document persistence (cursor position, scroll offset). The hash avoids
/// issues with deep paths or special characters while ensuring uniqueness.
fn doc_path_name(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::LapceWorkspaceType;

    /// Helper to create a LapceDb backed by a temporary directory.
    fn temp_db() -> (LapceDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = LapceDb::new_in(dir.path().to_path_buf()).unwrap();
        (db, dir)
    }

    // -- get_app / get_window min-size clamping tests --

    #[test]
    fn get_app_clamps_small_width() {
        let (db, _dir) = temp_db();
        let json = serde_json::json!({
            "windows": [{
                "size": {"width": 5.0, "height": 600.0},
                "pos": {"x": 0.0, "y": 0.0},
                "maximised": false,
                "tabs": {"active_tab": 0, "workspaces": []}
            }]
        });
        std::fs::write(db.folder.join(APP), json.to_string()).unwrap();
        let info = db.get_app().unwrap();
        assert_eq!(
            info.windows[0].size.width,
            LapceLayout::DEFAULT_WINDOW_WIDTH
        );
        assert_eq!(
            info.windows[0].size.height,
            LapceLayout::DEFAULT_WINDOW_HEIGHT
        );
    }

    #[test]
    fn get_app_clamps_small_height() {
        let (db, _dir) = temp_db();
        let json = serde_json::json!({
            "windows": [{
                "size": {"width": 1024.0, "height": 0.0},
                "pos": {"x": 0.0, "y": 0.0},
                "maximised": false,
                "tabs": {"active_tab": 0, "workspaces": []}
            }]
        });
        std::fs::write(db.folder.join(APP), json.to_string()).unwrap();
        let info = db.get_app().unwrap();
        assert_eq!(info.windows[0].size.width, 1024.0);
        assert_eq!(
            info.windows[0].size.height,
            LapceLayout::DEFAULT_WINDOW_HEIGHT
        );
    }

    #[test]
    fn get_app_preserves_normal_sizes() {
        let (db, _dir) = temp_db();
        let json = serde_json::json!({
            "windows": [{
                "size": {"width": 1200.0, "height": 800.0},
                "pos": {"x": 100.0, "y": 50.0},
                "maximised": false,
                "tabs": {"active_tab": 0, "workspaces": []}
            }]
        });
        std::fs::write(db.folder.join(APP), json.to_string()).unwrap();
        let info = db.get_app().unwrap();
        assert_eq!(info.windows[0].size.width, 1200.0);
        assert_eq!(info.windows[0].size.height, 800.0);
    }

    #[test]
    fn get_app_clamps_both_dimensions() {
        let (db, _dir) = temp_db();
        let json = serde_json::json!({
            "windows": [{
                "size": {"width": 0.0, "height": 0.0},
                "pos": {"x": 0.0, "y": 0.0},
                "maximised": false,
                "tabs": {"active_tab": 0, "workspaces": []}
            }]
        });
        std::fs::write(db.folder.join(APP), json.to_string()).unwrap();
        let info = db.get_app().unwrap();
        assert_eq!(
            info.windows[0].size.width,
            LapceLayout::DEFAULT_WINDOW_WIDTH
        );
        assert_eq!(
            info.windows[0].size.height,
            LapceLayout::DEFAULT_WINDOW_HEIGHT
        );
    }

    #[test]
    fn get_app_clamps_multiple_windows() {
        let (db, _dir) = temp_db();
        let json = serde_json::json!({
            "windows": [
                {
                    "size": {"width": 5.0, "height": 5.0},
                    "pos": {"x": 0.0, "y": 0.0},
                    "maximised": false,
                    "tabs": {"active_tab": 0, "workspaces": []}
                },
                {
                    "size": {"width": 1920.0, "height": 1080.0},
                    "pos": {"x": 100.0, "y": 100.0},
                    "maximised": true,
                    "tabs": {"active_tab": 0, "workspaces": []}
                }
            ]
        });
        std::fs::write(db.folder.join(APP), json.to_string()).unwrap();
        let info = db.get_app().unwrap();
        assert_eq!(
            info.windows[0].size.width,
            LapceLayout::DEFAULT_WINDOW_WIDTH
        );
        assert_eq!(
            info.windows[0].size.height,
            LapceLayout::DEFAULT_WINDOW_HEIGHT
        );
        assert_eq!(info.windows[1].size.width, 1920.0);
        assert_eq!(info.windows[1].size.height, 1080.0);
    }

    #[test]
    fn get_window_clamps_small_size() {
        let (db, _dir) = temp_db();
        let json = serde_json::json!({
            "size": {"width": 3.0, "height": 2.0},
            "pos": {"x": 0.0, "y": 0.0},
            "maximised": false,
            "tabs": {"active_tab": 0, "workspaces": []}
        });
        std::fs::write(db.folder.join(WINDOW), json.to_string()).unwrap();
        let info = db.get_window().unwrap();
        assert_eq!(info.size.width, LapceLayout::DEFAULT_WINDOW_WIDTH);
        assert_eq!(info.size.height, LapceLayout::DEFAULT_WINDOW_HEIGHT);
    }

    #[test]
    fn get_window_preserves_normal_size() {
        let (db, _dir) = temp_db();
        let json = serde_json::json!({
            "size": {"width": 1440.0, "height": 900.0},
            "pos": {"x": 50.0, "y": 50.0},
            "maximised": false,
            "tabs": {"active_tab": 0, "workspaces": []}
        });
        std::fs::write(db.folder.join(WINDOW), json.to_string()).unwrap();
        let info = db.get_window().unwrap();
        assert_eq!(info.size.width, 1440.0);
        assert_eq!(info.size.height, 900.0);
    }

    #[test]
    fn get_window_boundary_value_at_10() {
        let (db, _dir) = temp_db();
        // Exactly 10.0 should NOT be clamped (< 10.0 triggers clamping)
        let json = serde_json::json!({
            "size": {"width": 10.0, "height": 10.0},
            "pos": {"x": 0.0, "y": 0.0},
            "maximised": false,
            "tabs": {"active_tab": 0, "workspaces": []}
        });
        std::fs::write(db.folder.join(WINDOW), json.to_string()).unwrap();
        let info = db.get_window().unwrap();
        assert_eq!(info.size.width, 10.0);
        assert_eq!(info.size.height, 10.0);
    }

    // -- insert_recent_workspace tests --

    #[test]
    fn insert_recent_workspace_adds_new_entry() {
        let (db, _dir) = temp_db();
        let ws = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/project/a")),
            last_open: 0,
        };
        db.insert_recent_workspace(ws).unwrap();
        let workspaces = db.recent_workspaces().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].path, Some(PathBuf::from("/project/a")));
        // last_open should be set to current time (non-zero)
        assert!(workspaces[0].last_open > 0);
    }

    #[test]
    fn insert_recent_workspace_updates_existing_entry() {
        let (db, _dir) = temp_db();
        let ws = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/project/a")),
            last_open: 0,
        };
        db.insert_recent_workspace(ws.clone()).unwrap();
        let first_open = db.recent_workspaces().unwrap()[0].last_open;

        // Insert same workspace again — should update, not duplicate
        std::thread::sleep(std::time::Duration::from_secs(1));
        db.insert_recent_workspace(ws).unwrap();
        let workspaces = db.recent_workspaces().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert!(workspaces[0].last_open >= first_open);
    }

    #[test]
    fn insert_recent_workspace_sorts_most_recent_first() {
        let (db, _dir) = temp_db();
        let ws_a = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/project/a")),
            last_open: 0,
        };
        let ws_b = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/project/b")),
            last_open: 0,
        };
        db.insert_recent_workspace(ws_a.clone()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        db.insert_recent_workspace(ws_b).unwrap();

        let workspaces = db.recent_workspaces().unwrap();
        assert_eq!(workspaces.len(), 2);
        // Most recently opened should be first
        assert_eq!(workspaces[0].path, Some(PathBuf::from("/project/b")));
        assert_eq!(workspaces[1].path, Some(PathBuf::from("/project/a")));
    }

    #[test]
    fn insert_recent_workspace_reopen_moves_to_front() {
        let (db, _dir) = temp_db();
        let ws_a = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/project/a")),
            last_open: 0,
        };
        let ws_b = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/project/b")),
            last_open: 0,
        };
        db.insert_recent_workspace(ws_a.clone()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        db.insert_recent_workspace(ws_b).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        // Re-open A — should move to front
        db.insert_recent_workspace(ws_a).unwrap();

        let workspaces = db.recent_workspaces().unwrap();
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[0].path, Some(PathBuf::from("/project/a")));
    }

    // -- Roundtrip persistence tests --

    #[test]
    fn roundtrip_doc_info() {
        let (db, _dir) = temp_db();
        let ws = LapceWorkspace {
            kind: LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/project/y")),
            last_open: 0,
        };
        let info = DocInfo {
            workspace: ws.clone(),
            path: PathBuf::from("/project/y/src/main.rs"),
            scroll_offset: (100.5, 200.0),
            cursor_offset: 42,
        };
        db.insert_doc(&info).unwrap();
        let loaded = db
            .get_doc_info(&ws, Path::new("/project/y/src/main.rs"))
            .unwrap();
        assert_eq!(loaded.cursor_offset, 42);
        assert_eq!(loaded.scroll_offset, (100.5, 200.0));
    }

    // -- existing pure-function tests --

    #[test]
    fn workspace_folder_name_encodes_local_workspace() {
        let ws = LapceWorkspace {
            kind: crate::workspace::LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/home/user/project")),
            last_open: 0,
        };
        let name = workspace_folder_name(&ws);
        // The Display impl produces "Local:/home/user/project"
        // URL-encoding turns the colon and slashes into percent-encoded form
        assert!(name.contains("Local"));
        assert!(name.contains("home"));
        assert!(name.contains("project"));
        // Colons and slashes should be percent-encoded
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
    }

    #[test]
    fn workspace_folder_name_empty_path() {
        let ws = LapceWorkspace::default();
        let name = workspace_folder_name(&ws);
        // "Local:" => URL-encoded
        assert!(name.contains("Local"));
        assert!(!name.contains(':'));
    }

    #[test]
    fn workspace_folder_name_is_deterministic() {
        let ws = LapceWorkspace {
            kind: crate::workspace::LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/some/path")),
            last_open: 42,
        };
        let name1 = workspace_folder_name(&ws);
        let name2 = workspace_folder_name(&ws);
        assert_eq!(name1, name2);
    }

    #[test]
    fn workspace_folder_name_different_paths_differ() {
        let ws1 = LapceWorkspace {
            kind: crate::workspace::LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/path/a")),
            last_open: 0,
        };
        let ws2 = LapceWorkspace {
            kind: crate::workspace::LapceWorkspaceType::Local,
            path: Some(PathBuf::from("/path/b")),
            last_open: 0,
        };
        assert_ne!(workspace_folder_name(&ws1), workspace_folder_name(&ws2));
    }

    #[test]
    fn doc_path_name_returns_hex_sha256() {
        let path = Path::new("/home/user/project/main.rs");
        let name = doc_path_name(path);
        // SHA-256 hex digest is 64 characters
        assert_eq!(name.len(), 64);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn doc_path_name_is_deterministic() {
        let path = Path::new("/some/file.rs");
        assert_eq!(doc_path_name(path), doc_path_name(path));
    }

    #[test]
    fn doc_path_name_different_paths_differ() {
        let a = doc_path_name(Path::new("/a.rs"));
        let b = doc_path_name(Path::new("/b.rs"));
        assert_ne!(a, b);
    }

    #[test]
    fn doc_path_name_known_hash() {
        // Verify against a known SHA-256 hash
        let path = Path::new("/test");
        let name = doc_path_name(path);
        let mut hasher = Sha256::new();
        hasher.update(b"/test");
        let expected = format!("{:x}", hasher.finalize());
        assert_eq!(name, expected);
    }
}
