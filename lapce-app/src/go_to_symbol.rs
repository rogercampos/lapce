use std::{
    ops::Range,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use floem::{
    View,
    ext_event::create_signal_from_channel,
    keyboard::Modifiers,
    peniko::kurbo::{Point, Size},
    reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    style::{CursorStyle, Display},
    unit::PxPctAuto,
    views::{
        Decorators, VirtualVector, container, label, scroll,
        scroll::PropagatePointerWheel, stack, svg, text, virtual_stack,
    },
};
use lapce_core::{command::FocusCommand, mode::Mode, selection::Selection};
use lapce_rpc::proxy::{ProxyResponse, SymbolInformationEntry};
use lapce_xi_rope::Rope;

use crate::{
    about::exclusive_popup,
    command::{CommandExecuted, CommandKind, LapceCommand},
    config::{color::LapceColor, icon::LapceIcons, layout::LapceLayout},
    doc::DocContent,
    editor::EditorData,
    editor::location::{EditorLocation, EditorPosition},
    keypress::KeyPressFocus,
    main_split::MainSplitData,
    resizable_container::resizable_container,
    text_input::TextInputBuilder,
    workspace::LapceWorkspace,
    workspace_data::{CommonData, Focus, WorkspaceData},
};

fn is_type_or_constant(kind: lsp_types::SymbolKind) -> bool {
    matches!(
        kind,
        lsp_types::SymbolKind::CLASS
            | lsp_types::SymbolKind::MODULE
            | lsp_types::SymbolKind::CONSTANT
            | lsp_types::SymbolKind::STRUCT
            | lsp_types::SymbolKind::ENUM
            | lsp_types::SymbolKind::INTERFACE
            | lsp_types::SymbolKind::NAMESPACE
            | lsp_types::SymbolKind::ENUM_MEMBER
    )
}

#[derive(Clone)]
pub struct GoToSymbolData {
    pub visible: RwSignal<bool>,
    pub index: RwSignal<usize>,
    pub input_editor: EditorData,
    pub symbols: RwSignal<Vec<SymbolInformationEntry>>,
    pub workspace: Arc<LapceWorkspace>,
    pub main_split: MainSplitData,
    pub common: Rc<CommonData>,
}

impl std::fmt::Debug for GoToSymbolData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoToSymbolData").finish()
    }
}

impl GoToSymbolData {
    pub fn new(
        cx: Scope,
        workspace: Arc<LapceWorkspace>,
        main_split: MainSplitData,
        common: Rc<CommonData>,
    ) -> Self {
        let visible = cx.create_rw_signal(false);
        let index = cx.create_rw_signal(0usize);
        let symbols = cx.create_rw_signal(Vec::<SymbolInformationEntry>::new());
        let input_editor = main_split.editors.make_local(cx, common.clone());
        let query_rev = Arc::new(AtomicUsize::new(0));

        // Channel for receiving symbol results from proxy callbacks.
        // Using a channel avoids creating a new `create_ext_action` scope
        // per keystroke (which would leak when debounce skips the request).
        let (result_tx, result_rx) =
            std::sync::mpsc::channel::<Vec<SymbolInformationEntry>>();
        let result_signal = create_signal_from_channel(result_rx);

        {
            cx.create_effect(move |_| {
                if let Some(syms) = result_signal.get() {
                    let filtered: Vec<_> = syms
                        .into_iter()
                        .filter(|s| is_type_or_constant(s.kind))
                        .filter(|s| {
                            let path = s.location.uri.path();
                            !path.ends_with(".rbi") && !path.ends_with(".rbs")
                        })
                        .collect();
                    symbols.set(filtered);
                }
            });
        }

        // Watch input buffer changes → debounce → send workspace symbol request
        {
            let doc = input_editor.doc();
            let buffer = doc.buffer;
            let proxy = common.proxy.clone();
            let query_rev = query_rev.clone();
            let main_split = main_split.clone();

            cx.create_effect(move |prev: Option<String>| {
                let content = buffer.with(|b| b.to_string());

                // Deduplicate: the effect can fire multiple times for
                // the same buffer content; skip if nothing changed.
                if prev.as_deref() == Some(content.as_str()) {
                    return content;
                }

                let rev = query_rev.fetch_add(1, Ordering::SeqCst) + 1;

                if content.is_empty() {
                    symbols.set(Vec::new());
                    return content;
                }

                // Get an active file path to route to the correct LSP
                let path =
                    main_split.active_editor.get_untracked().and_then(|editor| {
                        let doc = editor.doc();
                        match doc.content.get_untracked() {
                            DocContent::File { path, .. } => Some(path),
                            _ => None,
                        }
                    });

                let Some(path) = path else {
                    return content;
                };

                // Debounce: wait 150ms before sending the request.
                // If another keystroke arrives in that window, this
                // thread exits early (query_rev will have advanced).
                let query_rev = query_rev.clone();
                let proxy = proxy.clone();
                let result_tx = result_tx.clone();
                let query = content.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(150));
                    if query_rev.load(Ordering::SeqCst) != rev {
                        return;
                    }
                    proxy.get_workspace_symbols(path, query, move |result| {
                        if let Ok(ProxyResponse::GetWorkspaceSymbolsResponse {
                            symbols,
                        }) = result
                        {
                            let _ = result_tx.send(symbols);
                        }
                    });
                });

                content
            });
        }

        // Reset index when symbols change
        {
            cx.create_effect(move |_| {
                let _ = symbols.get();
                index.set(0);
            });
        }

        // Auto-close when focus changes away
        {
            let focus = common.focus;
            cx.create_effect(move |_| {
                let f = focus.get();
                if f != Focus::GoToSymbol && visible.get_untracked() {
                    visible.set(false);
                }
            });
        }

        Self {
            visible,
            index,
            input_editor,
            symbols,
            workspace,
            main_split,
            common,
        }
    }

    pub fn open(&self) {
        self.input_editor.doc().reload(Rope::from(""), true);
        self.input_editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::caret(0)));
        self.index.set(0);
        self.symbols.set(Vec::new());
        self.visible.set(true);
        self.common.focus.set(Focus::GoToSymbol);
    }

    pub fn close(&self) {
        self.visible.set(false);
        if self.common.focus.get_untracked() == Focus::GoToSymbol {
            self.common.focus.set(Focus::Workbench);
        }
    }

    pub fn select(&self) {
        let symbols = self.symbols.get_untracked();
        let idx = self.index.get_untracked();
        if let Some(symbol) = symbols.get(idx) {
            if let Ok(path) = symbol.location.uri.to_file_path() {
                self.main_split.jump_to_location(
                    EditorLocation {
                        path,
                        position: Some(EditorPosition::Position(
                            symbol.location.range.start,
                        )),
                        scroll_offset: None,
                        same_editor_tab: false,
                    },
                    None,
                );
            }
        }
        self.close();
    }

    fn next(&self) {
        let len = self.symbols.with_untracked(|s| s.len());
        if len == 0 {
            return;
        }
        let index = self.index.get_untracked();
        if index + 1 < len {
            self.index.set(index + 1);
        }
    }

    fn previous(&self) {
        let index = self.index.get_untracked();
        if index > 0 {
            self.index.set(index - 1);
        }
    }
}

impl KeyPressFocus for GoToSymbolData {
    fn get_mode(&self) -> Mode {
        Mode::Insert
    }

    fn check_condition(
        &self,
        condition: crate::keypress::condition::Condition,
    ) -> bool {
        matches!(
            condition,
            crate::keypress::condition::Condition::ListFocus
                | crate::keypress::condition::Condition::ModalFocus
        )
    }

    fn run_command(
        &self,
        command: &LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Focus(cmd) => match cmd {
                FocusCommand::ModalClose => self.close(),
                FocusCommand::ListNext => self.next(),
                FocusCommand::ListPrevious => self.previous(),
                FocusCommand::ListSelect => self.select(),
                _ => return CommandExecuted::No,
            },
            _ => {
                self.input_editor.run_command(command, count, mods);
            }
        }
        CommandExecuted::Yes
    }

    fn receive_char(&self, c: &str) {
        self.input_editor.receive_char(c);
    }

    fn focus_only(&self) -> bool {
        true
    }
}

// -- View --

struct SymbolItems(Vec<SymbolInformationEntry>);

impl VirtualVector<(usize, SymbolInformationEntry)> for SymbolItems {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, SymbolInformationEntry)> {
        let start = range.start;
        let end = range.end.min(self.0.len());
        let start = start.min(end);
        self.0[start..end]
            .iter()
            .cloned()
            .enumerate()
            .map(move |(i, item)| (i + start, item))
    }
}

pub fn go_to_symbol_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.go_to_symbol_data.clone();
    let config = workspace_data.common.config;
    let visibility = data.visible;
    let close_data = data.clone();

    exclusive_popup(
        config,
        visibility,
        move || close_data.close(),
        move || go_to_symbol_content(workspace_data),
    )
    .debug_name("Go To Symbol Popup")
}

fn go_to_symbol_content(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.go_to_symbol_data.clone();
    let config = workspace_data.common.config;
    let focus = workspace_data.common.focus;
    let index = data.index;
    let symbols = data.symbols;
    let workspace_path = workspace_data.workspace.path.clone();
    let item_height = 30.0;

    let content = stack((
        go_to_symbol_input(data.clone(), config, focus),
        scroll({
            let data = data.clone();
            let workspace_path = workspace_path.clone();
            virtual_stack(
                move || SymbolItems(symbols.get()),
                move |(i, sym)| (*i, sym.name.clone()),
                move |(i, sym)| {
                    let data = data.clone();
                    let kind = sym.kind;
                    let name = sym.name.clone();
                    let container_name =
                        sym.container_name.clone().unwrap_or_default();
                    let file_path = sym
                        .location
                        .uri
                        .to_file_path()
                        .ok()
                        .and_then(|p| {
                            workspace_path
                                .as_ref()
                                .and_then(|ws| {
                                    p.strip_prefix(ws)
                                        .ok()
                                        .map(|r| r.to_string_lossy().to_string())
                                })
                                .or_else(|| Some(p.to_string_lossy().to_string()))
                        })
                        .unwrap_or_default();

                    let row = stack((
                        svg(move || {
                            let config = config.get();
                            config
                                .symbol_svg(&kind)
                                .unwrap_or_else(|| config.ui_svg(LapceIcons::FILE))
                        })
                        .style(move |s| {
                            let config = config.get();
                            let size = config.ui.icon_size() as f32;
                            s.min_width(size)
                                .size(size, size)
                                .margin_right(5.0)
                                .color(config.symbol_color(&kind).unwrap_or_else(
                                    || config.color(LapceColor::LAPCE_ICON_ACTIVE),
                                ))
                        }),
                        label(move || name.clone()).style(move |s| {
                            s.text_ellipsis().color(
                                config.get().color(LapceColor::EDITOR_FOREGROUND),
                            )
                        }),
                        label(move || {
                            if container_name.is_empty() {
                                String::new()
                            } else {
                                format!("  {container_name}")
                            }
                        })
                        .style(move |s| {
                            let config = config.get();
                            s.text_ellipsis()
                                .color(config.color(LapceColor::EDITOR_DIM))
                        }),
                        label(move || format!("  {file_path}")).style(move |s| {
                            let config = config.get();
                            s.margin_left(PxPctAuto::Auto)
                                .text_ellipsis()
                                .color(config.color(LapceColor::EDITOR_DIM))
                                .font_size(
                                    config.ui.font_size().saturating_sub(1) as f32
                                )
                        }),
                    ))
                    .style(|s| s.items_center().width_full());

                    container(row)
                        .on_click_stop(move |_| {
                            data.index.set(i);
                            data.select();
                        })
                        .style(move |s| {
                            let is_selected = index.get() == i;
                            let config = config.get();
                            s.width_full()
                                .height(item_height as f32)
                                .padding_horiz(10.0)
                                .items_center()
                                .cursor(CursorStyle::Pointer)
                                .apply_if(is_selected, |s| {
                                    s.background(config.color(
                                        LapceColor::PALETTE_CURRENT_BACKGROUND,
                                    ))
                                })
                                .hover(|s| {
                                    s.background(
                                        config.color(
                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                        ),
                                    )
                                })
                        })
                },
            )
            .item_size_fixed(move || item_height)
            .style(|s| s.width_full().flex_col())
        })
        .ensure_visible(move || {
            Size::new(1.0, item_height)
                .to_rect()
                .with_origin(Point::new(0.0, index.get() as f64 * item_height))
        })
        .style(|s| {
            s.width_full()
                .min_height(0.0)
                .flex_grow(1.0)
                .set(PropagatePointerWheel, false)
        }),
        text("No symbols found").style(move |s| {
            s.display(if symbols.with(|items| items.is_empty()) {
                Display::Flex
            } else {
                Display::None
            })
            .padding(10.0)
            .items_center()
            .height(item_height as f32)
            .color(config.get().color(LapceColor::EDITOR_DIM))
        }),
    ))
    .style(move |s| {
        let config = config.get();
        s.flex_col()
            .size_full()
            .border(1.0)
            .border_radius(LapceLayout::BORDER_RADIUS)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::PALETTE_BACKGROUND))
    });

    resizable_container(
        LapceLayout::DEFAULT_WINDOW_WIDTH,
        LapceLayout::DEFAULT_WINDOW_HEIGHT,
        400.0,
        300.0,
        content,
    )
}

fn go_to_symbol_input(
    data: GoToSymbolData,
    config: floem::reactive::ReadSignal<Arc<crate::config::LapceConfig>>,
    focus: RwSignal<Focus>,
) -> impl View {
    let is_focused = move || focus.get() == Focus::GoToSymbol;
    let input = TextInputBuilder::new()
        .is_focused(is_focused)
        .build_editor(data.input_editor.clone())
        .placeholder(|| "Search symbols...".to_owned())
        .style(|s| s.width_full());

    container(container(input).style(move |s| {
        let config = config.get();
        s.width_full()
            .height(30.0)
            .items_center()
            .border_bottom(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
    }))
    .style(|s| s.padding_bottom(5.0))
}
