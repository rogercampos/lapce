use floem::{
    action::show_context_menu,
    menu::{Menu, MenuItem},
};
use lapce_core::command::FocusCommand;

use crate::{
    command::{CommandKind, InternalCommand, LapceCommand, LapceWorkbenchCommand},
    editor_tab::EditorTabChild,
    id::EditorTabId,
    listener::Listener,
    main_split::TabCloseKind,
};

pub fn window_menu(
    lapce_command: Listener<LapceCommand>,
    workbench_command: Listener<LapceWorkbenchCommand>,
) -> Menu {
    let file_menu = Menu::new("File")
        .entry(MenuItem::new("Open Folder").action(move || {
            workbench_command.send(LapceWorkbenchCommand::OpenFolder);
        }))
        .entry(MenuItem::new("Open Recent Workspace").action(move || {
            workbench_command.send(LapceWorkbenchCommand::PaletteWorkspace);
        }));

    let view_menu = Menu::new("View")
        .entry(MenuItem::new("Toggle Left Panel").action(move || {
            workbench_command.send(LapceWorkbenchCommand::TogglePanelLeftVisual);
        }))
        .entry(MenuItem::new("Toggle Bottom Panel").action(move || {
            workbench_command.send(LapceWorkbenchCommand::TogglePanelBottomVisual);
        }))
        .entry(MenuItem::new("Toggle Right Panel").action(move || {
            workbench_command.send(LapceWorkbenchCommand::TogglePanelRightVisual);
        }))
        .separator()
        .entry(MenuItem::new("Toggle Inlay Hints").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ToggleInlayHints);
        }))
        .entry(MenuItem::new("Reset Zoom").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ZoomReset);
        }))
        .separator()
        .entry(MenuItem::new("Reveal Active File in File Explorer").action(
            move || {
                workbench_command
                    .send(LapceWorkbenchCommand::RevealActiveFileInFileExplorer);
            },
        ));

    let code_menu = Menu::new("Code")
        .entry(MenuItem::new("Go to Definition").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::GotoDefinition),
                data: None,
            });
        }))
        .entry(MenuItem::new("Go to Type Definition").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::GotoTypeDefinition),
                data: None,
            });
        }))
        .separator()
        .entry(MenuItem::new("Show Hover").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::ShowHover),
                data: None,
            });
        }))
        .entry(MenuItem::new("Show Code Actions").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::ShowCodeActions),
                data: None,
            });
        }))
        .separator()
        .entry(MenuItem::new("Rename Symbol").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::Rename),
                data: None,
            });
        }))
        .entry(MenuItem::new("Format Document").action(move || {
            lapce_command.send(LapceCommand {
                kind: CommandKind::Focus(FocusCommand::FormatDocument),
                data: None,
            });
        }));

    let window_menu = Menu::new("Window")
        .entry(MenuItem::new("New Window").action(move || {
            workbench_command.send(LapceWorkbenchCommand::NewWindow);
        }))
        .separator()
        .entry(MenuItem::new("Reload Window").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ReloadWindow);
        }));

    let settings_menu = Menu::new("Settings")
        .entry(MenuItem::new("Open Settings").action(move || {
            workbench_command.send(LapceWorkbenchCommand::OpenSettings);
        }))
        .entry(MenuItem::new("Open Keyboard Shortcuts").action(move || {
            workbench_command.send(LapceWorkbenchCommand::OpenKeyboardShortcuts);
        }))
        .separator()
        .entry(MenuItem::new("Change Color Theme").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ChangeColorTheme);
        }))
        .entry(MenuItem::new("Change Icon Theme").action(move || {
            workbench_command.send(LapceWorkbenchCommand::ChangeIconTheme);
        }))
        .entry(MenuItem::new("Open Theme Color Settings").action(move || {
            workbench_command.send(LapceWorkbenchCommand::OpenThemeColorSettings);
        }))
        .separator()
        .entry(MenuItem::new("Export Theme Settings").action(move || {
            workbench_command
                .send(LapceWorkbenchCommand::ExportCurrentThemeSettings);
        }))
        .entry(MenuItem::new("Install Theme").action(move || {
            workbench_command.send(LapceWorkbenchCommand::InstallTheme);
        }));

    let help_menu = {
        let mut menu = Menu::new("Help")
            .entry(MenuItem::new("Open Log File").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenLogFile);
            }))
            .entry(MenuItem::new("Open Logs Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenLogsDirectory);
            }))
            .separator()
            .entry(MenuItem::new("Open Settings Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenSettingsDirectory);
            }))
            .entry(MenuItem::new("Open Settings File").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenSettingsFile);
            }))
            .entry(
                MenuItem::new("Open Keyboard Shortcuts File").action(move || {
                    workbench_command
                        .send(LapceWorkbenchCommand::OpenKeyboardShortcutsFile);
                }),
            )
            .separator()
            .entry(MenuItem::new("Open Themes Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenThemesDirectory);
            }))
            .entry(MenuItem::new("Open Grammars Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenGrammarsDirectory);
            }))
            .entry(MenuItem::new("Open Queries Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenQueriesDirectory);
            }))
            .entry(MenuItem::new("Open Proxy Directory").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenProxyDirectory);
            }));
        #[cfg(target_os = "macos")]
        {
            menu = menu
                .separator()
                .entry(MenuItem::new("Install to PATH").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::InstallToPATH);
                }))
                .entry(MenuItem::new("Uninstall from PATH").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::UninstallFromPATH);
                }));
        }
        menu.separator()
            .entry(MenuItem::new("Show Environment").action(move || {
                workbench_command.send(LapceWorkbenchCommand::ShowEnvironment);
            }))
            .entry(MenuItem::new("Open UI Inspector").action(move || {
                workbench_command.send(LapceWorkbenchCommand::OpenUIInspector);
            }))
    };

    Menu::new("Lapce")
        .entry({
            let mut menu = Menu::new("Lapce")
                .entry(MenuItem::new("About Lapce").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::ShowAbout)
                }))
                .separator()
                .entry(MenuItem::new("Quit Lapce").action(move || {
                    workbench_command.send(LapceWorkbenchCommand::Quit);
                }));
            if cfg!(target_os = "macos") {
                menu = menu
                    .separator()
                    .entry(MenuItem::new("Hide Lapce"))
                    .entry(MenuItem::new("Hide Others"))
                    .entry(MenuItem::new("Show All"))
            }
            menu
        })
        .separator()
        .entry(file_menu)
        .entry(view_menu)
        .entry(code_menu)
        .entry(window_menu)
        .entry(settings_menu)
        .entry(help_menu)
}

pub(crate) fn tab_secondary_click(
    internal_command: Listener<InternalCommand>,
    editor_tab_id: EditorTabId,
    child: EditorTabChild,
) {
    let mut menu = Menu::new("");
    let child_other = child.clone();
    let child_right = child.clone();
    let child_left = child.clone();
    menu = menu
        .entry(MenuItem::new("Close").action(move || {
            internal_command.send(InternalCommand::EditorTabChildClose {
                editor_tab_id,
                child: child.clone(),
            });
        }))
        .entry(MenuItem::new("Close Other Tabs").action(move || {
            internal_command.send(InternalCommand::EditorTabCloseByKind {
                editor_tab_id,
                child: child_other.clone(),
                kind: TabCloseKind::CloseOther,
            });
        }))
        .entry(MenuItem::new("Close All Tabs").action(move || {
            internal_command.send(InternalCommand::EditorTabClose { editor_tab_id });
        }))
        .entry(MenuItem::new("Close Tabs to the Right").action(move || {
            internal_command.send(InternalCommand::EditorTabCloseByKind {
                editor_tab_id,
                child: child_right.clone(),
                kind: TabCloseKind::CloseToRight,
            });
        }))
        .entry(MenuItem::new("Close Tabs to the Left").action(move || {
            internal_command.send(InternalCommand::EditorTabCloseByKind {
                editor_tab_id,
                child: child_left.clone(),
                kind: TabCloseKind::CloseToLeft,
            });
        }));
    show_context_menu(menu, None);
}
