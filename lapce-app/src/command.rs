use std::{path::PathBuf, rc::Rc};

pub use floem::views::editor::command::CommandExecuted;
use floem::{peniko::kurbo::Vec2, views::editor::command::Command};
use indexmap::IndexMap;
use lapce_core::{
    command::{
        EditCommand, FocusCommand, MotionModeCommand, MoveCommand,
        MultiSelectionCommand, ScrollCommand,
    },
    language::LapceLanguage,
};
use lapce_rpc::plugin::PluginId;
use lsp_types::{CodeActionOrCommand, Location, Position, WorkspaceEdit};
use serde_json::Value;
use strum::{EnumMessage, IntoEnumIterator};
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

use crate::{
    alert::AlertButton,
    doc::Doc,
    editor::location::EditorLocation,
    editor_tab::EditorTabChild,
    id::EditorTabId,
    main_split::{SplitDirection, SplitMoveDirection, TabCloseKind},
    workspace::LapceWorkspace,
};

/// A command that can be executed by the application. Wraps a CommandKind variant
/// plus optional JSON data for parameterized commands (e.g. GoToLocation with a path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LapceCommand {
    pub kind: CommandKind,
    pub data: Option<Value>,
}

/// Unified command type that wraps all command families. Workbench commands are Lapce-specific,
/// while Edit/Move/Scroll/Focus/MotionMode/MultiSelection come from floem_editor_core
/// (re-exported via lapce-core). This allows the keypress system to resolve a single
/// keybinding to any kind of command regardless of origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Workbench(LapceWorkbenchCommand),
    Edit(EditCommand),
    Move(MoveCommand),
    Scroll(ScrollCommand),
    Focus(FocusCommand),
    MotionMode(MotionModeCommand),
    MultiSelection(MultiSelectionCommand),
}

impl CommandKind {
    pub fn desc(&self) -> Option<&'static str> {
        match &self {
            CommandKind::Workbench(cmd) => cmd.get_message(),
            CommandKind::Edit(cmd) => cmd.get_message(),
            CommandKind::Move(cmd) => cmd.get_message(),
            CommandKind::Scroll(cmd) => cmd.get_message(),
            CommandKind::Focus(cmd) => cmd.get_message(),
            CommandKind::MotionMode(cmd) => cmd.get_message(),
            CommandKind::MultiSelection(_) => None,
        }
    }

    pub fn str(&self) -> &'static str {
        match &self {
            CommandKind::Workbench(cmd) => cmd.into(),
            CommandKind::Edit(cmd) => cmd.into(),
            CommandKind::Move(cmd) => cmd.into(),
            CommandKind::Scroll(cmd) => cmd.into(),
            CommandKind::Focus(cmd) => cmd.into(),
            CommandKind::MotionMode(cmd) => cmd.into(),
            CommandKind::MultiSelection(_) => "",
        }
    }
}
impl From<Command> for CommandKind {
    fn from(cmd: Command) -> Self {
        use Command::*;
        match cmd {
            Edit(edit) => CommandKind::Edit(edit),
            Move(movement) => CommandKind::Move(movement),
            Scroll(scroll) => CommandKind::Scroll(scroll),
            MotionMode(motion_mode) => CommandKind::MotionMode(motion_mode),
            MultiSelection(multi_selection) => {
                CommandKind::MultiSelection(multi_selection)
            }
        }
    }
}

/// Build a registry of all available commands, keyed by their strum serialization string.
/// This registry is used by the palette (command search) and keybinding resolution.
/// Only commands registered here appear in the palette and can be bound to keys.
pub fn lapce_internal_commands() -> IndexMap<String, LapceCommand> {
    let mut commands = IndexMap::new();

    for c in LapceWorkbenchCommand::iter() {
        let command = LapceCommand {
            kind: CommandKind::Workbench(c.clone()),
            data: None,
        };
        commands.insert(c.to_string(), command);
    }

    for c in EditCommand::iter() {
        let command = LapceCommand {
            kind: CommandKind::Edit(c.clone()),
            data: None,
        };
        commands.insert(c.to_string(), command);
    }

    for c in MoveCommand::iter() {
        let command = LapceCommand {
            kind: CommandKind::Move(c.clone()),
            data: None,
        };
        commands.insert(c.to_string(), command);
    }

    for c in ScrollCommand::iter() {
        let command = LapceCommand {
            kind: CommandKind::Scroll(c.clone()),
            data: None,
        };
        commands.insert(c.to_string(), command);
    }

    for c in FocusCommand::iter() {
        let command = LapceCommand {
            kind: CommandKind::Focus(c.clone()),
            data: None,
        };
        commands.insert(c.to_string(), command);
    }

    commands
}

#[derive(
    Display,
    EnumString,
    EnumIter,
    Clone,
    PartialEq,
    Eq,
    Debug,
    EnumMessage,
    IntoStaticStr,
)]
pub enum LapceWorkbenchCommand {
    #[strum(serialize = "open_folder")]
    #[strum(message = "Open Folder")]
    OpenFolder,

    #[strum(serialize = "reveal_in_panel")]
    #[strum(message = "Reveal in Panel")]
    RevealInPanel,

    #[cfg(not(target_os = "macos"))]
    #[strum(serialize = "reveal_in_file_explorer")]
    #[strum(message = "Reveal in System File Explorer")]
    RevealInFileExplorer,

    #[cfg(target_os = "macos")]
    #[strum(serialize = "reveal_in_file_explorer")]
    #[strum(message = "Reveal in Finder")]
    RevealInFileExplorer,

    #[strum(serialize = "reveal_active_file_in_file_explorer")]
    #[strum(message = "Reveal Active File in File Explorer")]
    RevealActiveFileInFileExplorer,

    #[strum(serialize = "copy_file_path")]
    #[strum(message = "Copy File Path")]
    CopyFilePath,

    #[strum(serialize = "open_ui_inspector")]
    #[strum(message = "Open Internal UI Inspector")]
    OpenUIInspector,

    #[strum(serialize = "show_env")]
    #[strum(message = "Show Environment")]
    ShowEnvironment,

    #[strum(serialize = "open_settings")]
    #[strum(message = "Open Settings")]
    OpenSettings,

    #[strum(serialize = "open_settings_file")]
    #[strum(message = "Open Settings File")]
    OpenSettingsFile,

    #[strum(serialize = "open_settings_directory")]
    #[strum(message = "Open Settings Directory")]
    OpenSettingsDirectory,

    #[strum(serialize = "open_keyboard_shortcuts")]
    #[strum(message = "Open Keyboard Shortcuts")]
    OpenKeyboardShortcuts,

    #[strum(serialize = "open_keyboard_shortcuts_file")]
    #[strum(message = "Open Keyboard Shortcuts File")]
    OpenKeyboardShortcutsFile,

    #[strum(serialize = "open_log_file")]
    #[strum(message = "Open Log File")]
    OpenLogFile,

    #[strum(serialize = "open_logs_directory")]
    #[strum(message = "Open Logs Directory")]
    OpenLogsDirectory,

    #[strum(serialize = "open_proxy_directory")]
    #[strum(message = "Open Proxy Directory")]
    OpenProxyDirectory,

    #[strum(serialize = "open_grammars_directory")]
    #[strum(message = "Open Grammars Directory")]
    OpenGrammarsDirectory,

    #[strum(serialize = "open_queries_directory")]
    #[strum(message = "Open Queries Directory")]
    OpenQueriesDirectory,

    #[strum(serialize = "zoom_in")]
    #[strum(message = "Zoom In")]
    ZoomIn,

    #[strum(serialize = "zoom_out")]
    #[strum(message = "Zoom Out")]
    ZoomOut,

    #[strum(serialize = "zoom_reset")]
    #[strum(message = "Reset Zoom")]
    ZoomReset,

    #[strum(serialize = "reload_window")]
    #[strum(message = "Reload Window")]
    ReloadWindow,

    #[strum(message = "New Window")]
    #[strum(serialize = "new_window")]
    NewWindow,

    #[strum(message = "New File")]
    #[strum(serialize = "new_file")]
    NewFile,

    #[strum(message = "Go To Line")]
    #[strum(serialize = "go_to_line")]
    GoToLine,

    #[strum(serialize = "go_to_file")]
    #[strum(message = "Go To File")]
    GoToFile,

    #[strum(serialize = "toggle_maximized_panel")]
    ToggleMaximizedPanel,

    #[strum(serialize = "hide_panel")]
    HidePanel,

    #[strum(serialize = "show_panel")]
    ShowPanel,

    /// Toggles the panel passed in parameter.
    #[strum(serialize = "toggle_panel_focus")]
    TogglePanelFocus,

    /// Toggles the panel passed in parameter.
    #[strum(serialize = "toggle_panel_visual")]
    TogglePanelVisual,

    #[strum(message = "Toggle Left Panel")]
    #[strum(serialize = "toggle_panel_left_visual")]
    TogglePanelLeftVisual,

    #[strum(message = "Toggle Right Panel")]
    #[strum(serialize = "toggle_panel_right_visual")]
    TogglePanelRightVisual,

    #[strum(message = "Toggle Bottom Panel")]
    #[strum(serialize = "toggle_panel_bottom_visual")]
    TogglePanelBottomVisual,

    #[strum(message = "Toggle Search Focus")]
    #[strum(serialize = "toggle_search_focus")]
    ToggleSearchFocus,

    #[strum(serialize = "search_modal_open_full_results")]
    SearchModalOpenFullResults,

    #[strum(message = "Global Replace")]
    #[strum(serialize = "global_replace")]
    GlobalReplace,

    #[strum(serialize = "focus_editor")]
    FocusEditor,

    #[strum(serialize = "next_editor_tab")]
    #[strum(message = "Next Editor Tab")]
    NextEditorTab,

    #[strum(serialize = "previous_editor_tab")]
    #[strum(message = "Previous Editor Tab")]
    PreviousEditorTab,

    #[strum(serialize = "toggle_inlay_hints")]
    #[strum(message = "Toggle Inlay Hints")]
    ToggleInlayHints,

    #[strum(serialize = "restart_to_update")]
    RestartToUpdate,

    #[strum(serialize = "recent_files")]
    #[strum(message = "Recent Files")]
    RecentFiles,

    #[strum(serialize = "show_about")]
    #[strum(message = "About Lapce")]
    ShowAbout,

    #[cfg(target_os = "macos")]
    #[strum(message = "Install Lapce to PATH")]
    #[strum(serialize = "install_to_path")]
    InstallToPATH,

    #[cfg(target_os = "macos")]
    #[strum(message = "Uninstall Lapce from PATH")]
    #[strum(serialize = "uninstall_from_path")]
    UninstallFromPATH,

    #[strum(serialize = "jump_location_backward")]
    JumpLocationBackward,

    #[strum(serialize = "jump_location_forward")]
    JumpLocationForward,

    #[strum(serialize = "jump_location_backward_local")]
    JumpLocationBackwardLocal,

    #[strum(serialize = "jump_location_forward_local")]
    JumpLocationForwardLocal,

    #[strum(serialize = "quit")]
    #[strum(message = "Quit Editor")]
    Quit,

    #[strum(serialize = "go_to_location")]
    #[strum(message = "Go to Location")]
    GoToLocation,

    #[strum(serialize = "go_to_symbol")]
    #[strum(message = "Go to Symbol in Workspace")]
    GoToSymbol,
}

/// Internal commands are sent between components via the internal_command Listener.
/// Unlike LapceWorkbenchCommand (which appears in the palette and keybindings),
/// these are programmatic-only commands carrying rich data payloads.
#[derive(Clone, Debug)]
pub enum InternalCommand {
    ReloadConfig,
    OpenSearchPanel,
    OpenFile {
        path: PathBuf,
    },
    OpenFileInNewTab {
        path: PathBuf,
    },
    ReloadFileExplorer,
    /// Test whether a file/directory can be created at that path
    TestPathCreation {
        new_path: PathBuf,
    },
    FinishRenamePath {
        current_path: PathBuf,
        new_path: PathBuf,
    },
    FinishNewNode {
        is_dir: bool,
        path: PathBuf,
    },
    FinishDuplicate {
        source: PathBuf,
        path: PathBuf,
    },
    GoToLocation {
        location: EditorLocation,
    },
    JumpToLocation {
        location: EditorLocation,
    },
    SaveJumpLocation {
        path: PathBuf,
        offset: usize,
        scroll_offset: Vec2,
    },
    Split {
        direction: SplitDirection,
        editor_tab_id: EditorTabId,
    },
    SplitMove {
        direction: SplitMoveDirection,
        editor_tab_id: EditorTabId,
    },
    SplitExchange {
        editor_tab_id: EditorTabId,
    },
    EditorTabClose {
        editor_tab_id: EditorTabId,
    },
    EditorTabChildClose {
        editor_tab_id: EditorTabId,
        child: EditorTabChild,
    },
    EditorTabCloseByKind {
        editor_tab_id: EditorTabId,
        child: EditorTabChild,
        kind: TabCloseKind,
    },
    ShowCodeActions {
        offset: usize,
        mouse_click: bool,
        plugin_id: PluginId,
        code_actions: im::Vector<CodeActionOrCommand>,
    },
    ShowDefinitionPicker {
        offset: usize,
        locations: Vec<Location>,
        language: LapceLanguage,
    },
    RunCodeAction {
        plugin_id: PluginId,
        action: CodeActionOrCommand,
    },
    ApplyWorkspaceEdit {
        edit: WorkspaceEdit,
    },
    StartRename {
        path: PathBuf,
        placeholder: String,
        start: usize,
        position: Position,
    },
    FocusEditorTab {
        editor_tab_id: EditorTabId,
    },

    UpdateLogLevel {
        level: tracing_subscriber::filter::LevelFilter,
    },
    OpenWebUri {
        uri: String,
    },
    ShowAlert {
        title: String,
        msg: String,
        buttons: Vec<AlertButton>,
    },
    HideAlert,
    SaveScratchDoc {
        doc: Rc<Doc>,
    },
    ResetBlinkCursor,
    ExecuteProcess {
        program: String,
        arguments: Vec<String>,
    },
    TrackRecentFile {
        path: PathBuf,
    },
    CloseSearchTab {
        index: usize,
    },
    CloseAllSearchTabs,
}

#[derive(Clone)]
pub enum WindowCommand {
    SetWorkspace { workspace: LapceWorkspace },
    NewWindow,
    CloseWindow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lapce_internal_commands_is_nonempty() {
        let cmds = lapce_internal_commands();
        assert!(!cmds.is_empty());
    }

    #[test]
    fn lapce_internal_commands_no_duplicate_keys() {
        let cmds = lapce_internal_commands();
        let mut seen = std::collections::HashSet::new();
        for key in cmds.keys() {
            assert!(seen.insert(key.clone()), "duplicate key: {key}");
        }
    }

    #[test]
    fn lapce_internal_commands_contains_workbench_commands() {
        let cmds = lapce_internal_commands();
        // Spot-check known workbench commands
        assert!(cmds.contains_key("open_folder"));
        assert!(cmds.contains_key("go_to_file"));
        assert!(cmds.contains_key("quit"));
        assert!(cmds.contains_key("new_file"));
        assert!(cmds.contains_key("zoom_in"));
        assert!(cmds.contains_key("zoom_out"));
    }

    #[test]
    fn lapce_internal_commands_contains_edit_commands() {
        let cmds = lapce_internal_commands();
        // EditCommand variants have strum serialization
        for c in EditCommand::iter() {
            let key = c.to_string();
            assert!(cmds.contains_key(&key), "missing EditCommand: {key}");
            assert!(
                matches!(cmds[&key].kind, CommandKind::Edit(_)),
                "wrong kind for {key}"
            );
        }
    }

    #[test]
    fn lapce_internal_commands_contains_move_commands() {
        let cmds = lapce_internal_commands();
        for c in MoveCommand::iter() {
            let key = c.to_string();
            assert!(cmds.contains_key(&key), "missing MoveCommand: {key}");
        }
    }

    #[test]
    fn lapce_internal_commands_contains_scroll_commands() {
        let cmds = lapce_internal_commands();
        for c in ScrollCommand::iter() {
            let key = c.to_string();
            assert!(cmds.contains_key(&key), "missing ScrollCommand: {key}");
        }
    }

    #[test]
    fn lapce_internal_commands_contains_focus_commands() {
        let cmds = lapce_internal_commands();
        for c in FocusCommand::iter() {
            let key = c.to_string();
            assert!(cmds.contains_key(&key), "missing FocusCommand: {key}");
        }
    }

    #[test]
    fn lapce_internal_commands_all_have_none_data() {
        let cmds = lapce_internal_commands();
        for (key, cmd) in &cmds {
            assert!(cmd.data.is_none(), "command {key} should have data=None");
        }
    }

    #[test]
    fn command_kind_desc_workbench_with_message() {
        let kind = CommandKind::Workbench(LapceWorkbenchCommand::OpenFolder);
        assert_eq!(kind.desc(), Some("Open Folder"));
    }

    #[test]
    fn command_kind_desc_multi_selection_is_none() {
        let kind =
            CommandKind::MultiSelection(MultiSelectionCommand::SelectAllCurrent);
        assert_eq!(kind.desc(), None);
    }

    #[test]
    fn command_kind_str_workbench() {
        let kind = CommandKind::Workbench(LapceWorkbenchCommand::Quit);
        assert_eq!(kind.str(), "quit");
    }

    #[test]
    fn command_kind_str_multi_selection_is_empty() {
        let kind =
            CommandKind::MultiSelection(MultiSelectionCommand::SelectAllCurrent);
        assert_eq!(kind.str(), "");
    }

    #[test]
    fn command_kind_from_edit() {
        let cmd = Command::Edit(EditCommand::Undo);
        let kind = CommandKind::from(cmd);
        assert!(matches!(kind, CommandKind::Edit(EditCommand::Undo)));
    }

    #[test]
    fn command_kind_from_move() {
        let cmd = Command::Move(MoveCommand::Up);
        let kind = CommandKind::from(cmd);
        assert!(matches!(kind, CommandKind::Move(MoveCommand::Up)));
    }

    #[test]
    fn command_kind_from_scroll() {
        let cmd = Command::Scroll(ScrollCommand::PageUp);
        let kind = CommandKind::from(cmd);
        assert!(matches!(kind, CommandKind::Scroll(ScrollCommand::PageUp)));
    }

    #[test]
    fn command_kind_from_motion_mode() {
        let cmd = Command::MotionMode(MotionModeCommand::MotionModeDelete);
        let kind = CommandKind::from(cmd);
        assert!(matches!(
            kind,
            CommandKind::MotionMode(MotionModeCommand::MotionModeDelete)
        ));
    }

    #[test]
    fn command_kind_from_multi_selection() {
        let cmd = Command::MultiSelection(MultiSelectionCommand::SelectAllCurrent);
        let kind = CommandKind::from(cmd);
        assert!(matches!(
            kind,
            CommandKind::MultiSelection(MultiSelectionCommand::SelectAllCurrent)
        ));
    }
}
