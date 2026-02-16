use std::{
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, Sender, TryRecvError, channel},
    },
};

use floem::{
    View,
    ext_event::{create_ext_action, create_signal_from_channel},
    keyboard::Modifiers,
    peniko::kurbo::{Point, Size},
    reactive::{ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    style::{AlignItems, CursorStyle, Display},
    views::{
        Decorators, VirtualVector, container, scroll, scroll::PropagatePointerWheel,
        stack, svg, text, virtual_stack,
    },
};
use lapce_core::{
    command::FocusCommand, mode::Mode, movement::Movement, selection::Selection,
};
use lapce_rpc::proxy::ProxyResponse;
use lapce_xi_rope::Rope;
use nucleo::Utf32Str;

use crate::{
    about::exclusive_popup,
    command::{CommandExecuted, CommandKind, InternalCommand, LapceCommand},
    config::{LapceConfig, color::LapceColor},
    editor::EditorData,
    focus_text::focus_text,
    keypress::KeyPressFocus,
    main_split::MainSplitData,
    text_input::TextInputBuilder,
    workspace::LapceWorkspace,
    workspace_data::{CommonData, Focus, WorkspaceData},
};

#[derive(Clone, Debug, PartialEq)]
pub struct GoToFileItem {
    pub path: PathBuf,
    pub full_path: PathBuf,
    pub filter_text: String,
    pub score: u32,
    pub indices: Vec<usize>,
}

#[derive(Clone)]
pub struct GoToFileData {
    run_id_counter: Arc<AtomicU64>,
    pub run_id: RwSignal<u64>,
    pub visible: RwSignal<bool>,
    pub index: RwSignal<usize>,
    pub items: RwSignal<im::Vector<GoToFileItem>>,
    pub filtered_items: ReadSignal<im::Vector<GoToFileItem>>,
    pub filter_text: RwSignal<String>,
    pub input_editor: EditorData,
    pub workspace: Arc<LapceWorkspace>,
    pub main_split: MainSplitData,
    pub common: Rc<CommonData>,
}

impl std::fmt::Debug for GoToFileData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoToFileData").finish()
    }
}

impl GoToFileData {
    pub fn new(
        cx: Scope,
        workspace: Arc<LapceWorkspace>,
        main_split: MainSplitData,
        common: Rc<CommonData>,
    ) -> Self {
        let visible = cx.create_rw_signal(false);
        let index = cx.create_rw_signal(0usize);
        let items = cx.create_rw_signal(im::Vector::new());
        let run_id = cx.create_rw_signal(0u64);
        let run_id_counter = Arc::new(AtomicU64::new(0));
        let input_editor = main_split.editors.make_local(cx, common.clone());

        let doc = input_editor.doc();
        let filter_text = cx.create_rw_signal(String::new());
        {
            let buffer = doc.buffer;
            cx.create_effect(move |_| {
                let content = buffer.with(|b| b.to_string());
                filter_text.set(content);
            });
        }

        // Two reactive effects feed a background thread that performs fuzzy filtering.
        let (run_tx, run_rx) = channel();
        {
            let run_id = run_id.read_only();
            let items = items.read_only();
            let filter_text = filter_text.read_only();
            let tx = run_tx;
            {
                let tx = tx.clone();
                // Triggers when items change (file list loaded from proxy).
                cx.create_effect(move |_| {
                    let items = items.get();
                    let input = filter_text.get_untracked();
                    let run_id = run_id.get_untracked();
                    if let Err(err) = tx.send((run_id, input, items)) {
                        tracing::error!("{:?}", err);
                    }
                });
            }
            // Triggers when user types in the input.
            cx.create_effect(move |last_input: Option<String>| {
                let input = filter_text.get();
                if last_input.as_deref() == Some(input.as_str()) {
                    return input;
                }
                let items = items.get_untracked();
                let run_id = run_id.get_untracked();
                if let Err(err) = tx.send((run_id, input.clone(), items)) {
                    tracing::error!("{:?}", err);
                }
                input
            });
        }

        // Background thread for fuzzy filtering.
        let (resp_tx, resp_rx) = channel();
        {
            let run_id = run_id_counter.clone();
            std::thread::Builder::new()
                .name("GoToFileFilterThread".to_owned())
                .spawn(move || {
                    Self::update_process(run_id, run_rx, resp_tx);
                })
                .unwrap();
        }

        // Receive filtered results from the background thread.
        let (filtered_items, set_filtered_items) =
            cx.create_signal(im::Vector::new());
        {
            let resp = create_signal_from_channel(resp_rx);
            let run_id = run_id.read_only();
            let filter_text = filter_text.read_only();
            cx.create_effect(move |_| {
                if let Some((filter_run_id, filter_input, new_items)) = resp.get() {
                    if run_id.get_untracked() == filter_run_id
                        && filter_text.get_untracked() == filter_input
                    {
                        set_filtered_items.set(new_items);
                        index.set(0);
                    }
                }
            });
        }

        // Auto-close when focus changes away.
        {
            let visible = visible;
            let focus = common.focus;
            cx.create_effect(move |_| {
                let f = focus.get();
                if f != Focus::GoToFile && visible.get_untracked() {
                    visible.set(false);
                }
            });
        }

        Self {
            run_id_counter,
            run_id,
            visible,
            index,
            items,
            filtered_items,
            filter_text,
            input_editor,
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
        self.visible.set(true);
        self.common.focus.set(Focus::GoToFile);

        let run_id = self.run_id_counter.fetch_add(1, Ordering::Relaxed) + 1;
        self.run_id.set(run_id);
        self.get_files();
    }

    pub fn close(&self) {
        self.visible.set(false);
        self.items.update(|items| items.clear());
        if self.common.focus.get_untracked() == Focus::GoToFile {
            self.common.focus.set(Focus::Workbench);
        }
    }

    pub fn select(&self) {
        let index = self.index.get_untracked();
        let items = self.filtered_items.get_untracked();
        self.close();
        if let Some(item) = items.get(index) {
            self.common
                .internal_command
                .send(InternalCommand::OpenFile {
                    path: item.full_path.clone(),
                });
        }
    }

    fn next(&self) {
        let index = self.index.get_untracked();
        let len = self.filtered_items.with_untracked(|i| i.len());
        let new_index = Movement::Down.update_index(index, len, 1, true);
        self.index.set(new_index);
    }

    fn previous(&self) {
        let index = self.index.get_untracked();
        let len = self.filtered_items.with_untracked(|i| i.len());
        let new_index = Movement::Up.update_index(index, len, 1, true);
        self.index.set(new_index);
    }

    fn get_files(&self) {
        let workspace = self.workspace.clone();
        let set_items = self.items.write_only();
        let send =
            create_ext_action(self.common.scope, move |items: Vec<PathBuf>| {
                let items = items
                    .into_iter()
                    .map(|full_path| {
                        let path =
                            if let Some(workspace_path) = workspace.path.as_ref() {
                                full_path
                                    .strip_prefix(workspace_path)
                                    .unwrap_or(&full_path)
                                    .to_path_buf()
                            } else {
                                full_path.clone()
                            };
                        let filter_text = path.to_string_lossy().into_owned();
                        GoToFileItem {
                            path,
                            full_path,
                            filter_text,
                            score: 0,
                            indices: Vec::new(),
                        }
                    })
                    .collect::<im::Vector<_>>();
                set_items.set(items);
            });
        self.common.proxy.get_files(move |result| {
            if let Ok(ProxyResponse::GetFilesResponse { items }) = result {
                send(items);
            }
        });
    }

    fn filter_items(
        run_id: Arc<AtomicU64>,
        current_run_id: u64,
        input: &str,
        items: im::Vector<GoToFileItem>,
        matcher: &mut nucleo::Matcher,
    ) -> Option<im::Vector<GoToFileItem>> {
        if input.is_empty() {
            return Some(items);
        }

        let pattern = nucleo::pattern::Pattern::parse(
            input,
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Smart,
        );

        let mut filtered_items = Vec::new();
        let mut indices = Vec::new();
        let mut filter_text_buf = Vec::new();
        for i in &items {
            if run_id.load(Ordering::Acquire) != current_run_id {
                return None;
            }

            indices.clear();
            filter_text_buf.clear();
            let filter_text = Utf32Str::new(&i.filter_text, &mut filter_text_buf);
            if let Some(score) = pattern.indices(filter_text, matcher, &mut indices)
            {
                let mut item = i.clone();
                item.score = score;
                item.indices = indices.iter().map(|i| *i as usize).collect();
                filtered_items.push(item);
            }
        }

        filtered_items.sort_by(|a, b| {
            let order = b.score.cmp(&a.score);
            match order {
                std::cmp::Ordering::Equal => a.filter_text.cmp(&b.filter_text),
                _ => order,
            }
        });

        if run_id.load(Ordering::Acquire) != current_run_id {
            return None;
        }
        Some(filtered_items.into())
    }

    fn update_process(
        run_id: Arc<AtomicU64>,
        receiver: Receiver<(u64, String, im::Vector<GoToFileItem>)>,
        resp_tx: Sender<(u64, String, im::Vector<GoToFileItem>)>,
    ) {
        fn receive_batch(
            receiver: &Receiver<(u64, String, im::Vector<GoToFileItem>)>,
        ) -> anyhow::Result<(u64, String, im::Vector<GoToFileItem>)> {
            let (mut run_id, mut input, mut items) = receiver.recv()?;
            loop {
                match receiver.try_recv() {
                    Ok(update) => {
                        run_id = update.0;
                        input = update.1;
                        items = update.2;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            Ok((run_id, input, items))
        }

        let mut matcher =
            nucleo::Matcher::new(nucleo::Config::DEFAULT.match_paths());
        loop {
            if let Ok((current_run_id, input, items)) = receive_batch(&receiver) {
                if let Some(filtered_items) = GoToFileData::filter_items(
                    run_id.clone(),
                    current_run_id,
                    &input,
                    items,
                    &mut matcher,
                ) {
                    if let Err(err) =
                        resp_tx.send((current_run_id, input, filtered_items))
                    {
                        tracing::error!("{:?}", err);
                    }
                }
            } else {
                return;
            }
        }
    }
}

impl KeyPressFocus for GoToFileData {
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

struct GoToFileItems(im::Vector<GoToFileItem>);

impl VirtualVector<(usize, GoToFileItem)> for GoToFileItems {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, GoToFileItem)> {
        let start = range.start;
        Box::new(
            self.0
                .slice(range)
                .into_iter()
                .enumerate()
                .map(move |(i, item)| (i + start, item)),
        )
    }
}

pub fn go_to_file_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.go_to_file_data.clone();
    let config = workspace_data.common.config;
    let visibility = data.visible;
    let close_data = data.clone();

    exclusive_popup(
        config,
        visibility,
        move || close_data.close(),
        move || go_to_file_content(workspace_data),
    )
    .debug_name("Go To File Popup")
}

fn go_to_file_content(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.go_to_file_data.clone();
    let config = workspace_data.common.config;
    let focus = workspace_data.common.focus;
    let layout_rect = workspace_data.layout_rect.read_only();
    let index = data.index;
    let filtered_items = data.filtered_items;
    let run_id = data.run_id;
    let filter_text = data.filter_text.read_only();
    let item_height = 25.0;

    stack((
        go_to_file_input(data.clone(), config, focus),
        scroll({
            let data = data.clone();
            virtual_stack(
                move || GoToFileItems(filtered_items.get()),
                move |(i, _item)| {
                    (run_id.get_untracked(), *i, filter_text.get_untracked())
                },
                move |(i, item)| {
                    let data = data.clone();
                    container(go_to_file_item_view(
                        i,
                        item,
                        index.read_only(),
                        item_height,
                        config,
                    ))
                    .on_click_stop(move |_| {
                        data.index.set(i);
                        data.select();
                    })
                    .style(move |s| {
                        s.width_full().cursor(CursorStyle::Pointer).hover(|s| {
                            s.background(
                                config
                                    .get()
                                    .color(LapceColor::PANEL_HOVERED_BACKGROUND),
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
                .set(PropagatePointerWheel, false)
        }),
        text("No matching results").style(move |s| {
            s.display(if filtered_items.with(|items| items.is_empty()) {
                Display::Flex
            } else {
                Display::None
            })
            .padding_horiz(10.0)
            .align_items(Some(AlignItems::Center))
            .height(item_height as f32)
        }),
    ))
    .style(move |s| {
        let config = config.get();
        s.flex_col()
            .width(config.ui.palette_width() as f64)
            .max_width_pct(80.0)
            .max_height((layout_rect.get().height() * 0.45).round() as f32)
            .border(1.0)
            .border_radius(6.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::PALETTE_BACKGROUND))
    })
}

fn go_to_file_input(
    data: GoToFileData,
    config: ReadSignal<Arc<LapceConfig>>,
    focus: RwSignal<Focus>,
) -> impl View {
    let is_focused = move || focus.get() == Focus::GoToFile;
    let input = TextInputBuilder::new()
        .is_focused(is_focused)
        .build_editor(data.input_editor.clone())
        .placeholder(|| "Go to file...".to_owned())
        .style(|s| s.width_full());

    container(container(input).style(move |s| {
        let config = config.get();
        s.width_full()
            .height(25.0)
            .items_center()
            .border_bottom(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
    }))
    .style(|s| s.padding_bottom(5.0))
}

fn go_to_file_item_view(
    i: usize,
    item: GoToFileItem,
    index: ReadSignal<usize>,
    item_height: f64,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let file_name = item
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let folder = item
        .path
        .parent()
        .map(|p| crate::path::display_path(p))
        .unwrap_or_default();
    let folder_len = folder.len();

    let file_name_indices = item
        .indices
        .iter()
        .filter_map(|&idx| {
            if folder_len > 0 {
                if idx > folder_len {
                    Some(idx - folder_len - 1)
                } else {
                    None
                }
            } else {
                Some(idx)
            }
        })
        .collect::<Vec<_>>();
    let folder_indices = item
        .indices
        .iter()
        .filter_map(|&idx| if idx < folder_len { Some(idx) } else { None })
        .collect::<Vec<_>>();

    let path = item.path.to_path_buf();
    let style_path = path.clone();
    container(
        stack((
            svg(move || config.get().file_svg(&path).0).style(move |s| {
                let config = config.get();
                let size = config.ui.icon_size() as f32;
                let color = config.file_svg(&style_path).1;
                s.min_width(size)
                    .size(size, size)
                    .margin_right(5.0)
                    .apply_opt(color, floem::style::Style::color)
            }),
            focus_text(
                move || file_name.clone(),
                move || file_name_indices.clone(),
                move || config.get().color(LapceColor::EDITOR_FOCUS),
            )
            .style(|s| s.margin_right(6.0).max_width_full()),
            focus_text(
                move || folder.clone(),
                move || folder_indices.clone(),
                move || config.get().color(LapceColor::EDITOR_FOCUS),
            )
            .style(move |s| {
                s.color(config.get().color(LapceColor::EDITOR_DIM))
                    .min_width(0.0)
                    .flex_grow(1.0)
                    .flex_basis(0.0)
            }),
        ))
        .style(|s| s.align_items(Some(AlignItems::Center)).max_width_full()),
    )
    .style(move |s| {
        s.width_full()
            .height(item_height as f32)
            .padding_horiz(10.0)
            .apply_if(index.get() == i, |style| {
                style.background(
                    config.get().color(LapceColor::PALETTE_CURRENT_BACKGROUND),
                )
            })
    })
}
