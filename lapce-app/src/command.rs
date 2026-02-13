use std::{path::PathBuf, rc::Rc};

pub use floem::views::editor::command::CommandExecuted;
use floem::{
    ViewId, keyboard::Modifiers, peniko::kurbo::Vec2,
    views::editor::command::Command,
};
use indexmap::IndexMap;
use lapce_core::command::{
    EditCommand, FocusCommand, MotionModeCommand, MoveCommand,
    MultiSelectionCommand, ScrollCommand,
};
use lapce_rpc::plugin::{PluginId, VoltID};
use lsp_types::{CodeActionOrCommand, Position, WorkspaceEdit};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LapceCommand {
    pub kind: CommandKind,
    pub data: Option<Value>,
}

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

    #[strum(serialize = "show_call_hierarchy")]
    #[strum(message = "Show Call Hierarchy")]
    ShowCallHierarchy,

    #[strum(serialize = "find_references")]
    #[strum(message = "Find References")]
    FindReferences,

    #[strum(serialize = "go_to_implementation")]
    #[strum(message = "Go to Implementation")]
    GoToImplementation,

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

    #[strum(serialize = "change_color_theme")]
    #[strum(message = "Change Color Theme")]
    ChangeColorTheme,

    #[strum(serialize = "change_icon_theme")]
    #[strum(message = "Change Icon Theme")]
    ChangeIconTheme,

    #[strum(serialize = "open_settings")]
    #[strum(message = "Open Settings")]
    OpenSettings,

    #[strum(serialize = "open_settings_file")]
    #[strum(message = "Open Settings File")]
    OpenSettingsFile,

    #[strum(serialize = "open_settings_directory")]
    #[strum(message = "Open Settings Directory")]
    OpenSettingsDirectory,

    #[strum(serialize = "open_theme_color_settings")]
    #[strum(message = "Open Theme Color Settings")]
    OpenThemeColorSettings,

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

    #[strum(serialize = "open_themes_directory")]
    #[strum(message = "Open Themes Directory")]
    OpenThemesDirectory,

    #[strum(serialize = "open_plugins_directory")]
    #[strum(message = "Open Plugins Directory")]
    OpenPluginsDirectory,

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
    #[strum(serialize = "palette.line")]
    PaletteLine,

    #[strum(serialize = "palette")]
    #[strum(message = "Go to File")]
    Palette,

    #[strum(message = "Open Recent Workspace")]
    #[strum(serialize = "palette.workspace")]
    PaletteWorkspace,

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

    #[strum(serialize = "search_modal_open_full_results")]
    SearchModalOpenFullResults,

    #[strum(serialize = "focus_editor")]
    FocusEditor,

    #[strum(serialize = "export_current_theme_settings")]
    #[strum(message = "Export current settings to a theme file")]
    ExportCurrentThemeSettings,

    #[strum(serialize = "install_theme")]
    #[strum(message = "Install current theme file")]
    InstallTheme,

    #[strum(serialize = "change_file_language")]
    #[strum(message = "Change current file language")]
    ChangeFileLanguage,

    #[strum(serialize = "change_file_line_ending")]
    #[strum(message = "Change current file line ending")]
    ChangeFileLineEnding,

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

    #[strum(serialize = "show_plugins")]
    #[strum(message = "Plugins")]
    ShowPlugins,

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
}

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
    PaletteReferences {
        references: Vec<EditorLocation>,
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
    Search {
        pattern: Option<String>,
    },
    FindEditorReceiveChar {
        s: String,
    },
    ReplaceEditorReceiveChar {
        s: String,
    },
    FindEditorCommand {
        command: LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    },
    ReplaceEditorCommand {
        command: LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    },
    FocusEditorTab {
        editor_tab_id: EditorTabId,
    },

    SetColorTheme {
        name: String,
        /// Whether to save the theme to the config file
        save: bool,
    },
    SetIconTheme {
        name: String,
        /// Whether to save the theme to the config file
        save: bool,
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
    SaveScratchDoc2 {
        doc: Rc<Doc>,
    },
    OpenVoltView {
        volt_id: VoltID,
    },
    ResetBlinkCursor,
    ExecuteProcess {
        program: String,
        arguments: Vec<String>,
    },
    CallHierarchyIncoming {
        item_id: ViewId,
    },
    TrackRecentFile {
        path: PathBuf,
    },
}

#[derive(Clone)]
pub enum WindowCommand {
    SetWorkspace { workspace: LapceWorkspace },
    NewWindow,
    CloseWindow,
}
