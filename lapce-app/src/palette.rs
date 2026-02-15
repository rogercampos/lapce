use std::{
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, Sender, TryRecvError, channel},
    },
};

use anyhow::Result;
use floem::{
    ext_event::{create_ext_action, create_signal_from_channel},
    keyboard::Modifiers,
    reactive::{
        ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        use_context,
    },
};
use itertools::Itertools;
use lapce_core::{
    buffer::rope_text::RopeText, command::FocusCommand, language::LapceLanguage,
    line_ending::LineEnding, movement::Movement, selection::Selection,
    syntax::Syntax,
};
use lapce_rpc::proxy::ProxyResponse;
use lapce_xi_rope::Rope;
use nucleo::Utf32Str;

use self::{
    item::{PaletteItem, PaletteItemContent},
    kind::PaletteKind,
};
use crate::{
    command::{CommandExecuted, CommandKind, InternalCommand, WindowCommand},
    db::LapceDb,
    editor::{
        EditorData, EditorViewKind,
        location::{EditorLocation, EditorPosition},
    },
    keypress::{KeyPressFocus, condition::Condition},
    main_split::MainSplitData,
    workspace::LapceWorkspace,
    workspace_data::{CommonData, Focus},
};

pub mod item;
pub mod kind;

/// Tracks the lifecycle of a palette operation. `Started` means items are being loaded
/// (e.g., file list from proxy), `Done` means filtering is complete and results are displayed.
#[derive(Clone, PartialEq, Eq)]
pub enum PaletteStatus {
    Inactive,
    Started,
}

#[derive(Clone, Debug)]
pub struct PaletteInput {
    pub input: String,
    pub kind: PaletteKind,
}

impl PaletteInput {
    /// Update the current input in the palette
    pub fn update_input(&mut self, input: String, kind: PaletteKind) {
        self.kind = kind;
        self.input = input;
    }
}

#[derive(Clone)]
pub struct PaletteData {
    /// Monotonically increasing counter shared with the background filter thread.
    /// Used to detect stale filter operations so they can bail out early.
    run_id_counter: Arc<AtomicU64>,
    /// The current run_id on the UI side; compared against filter results to discard stale responses.
    pub run_id: RwSignal<u64>,
    pub workspace: Arc<LapceWorkspace>,
    pub status: RwSignal<PaletteStatus>,
    /// The currently highlighted/selected index in the filtered items list.
    pub index: RwSignal<usize>,
    /// When set, the filter thread will use this index instead of 0 for the initial selection.
    /// Used by theme/language pickers to pre-select the currently active value.
    pub preselect_index: RwSignal<Option<usize>>,
    /// The raw unfiltered items populated by the palette kind's data-loading function.
    pub items: RwSignal<im::Vector<PaletteItem>>,
    /// Derived from `items` via the background fuzzy-filter thread; this is what the view renders.
    pub filtered_items: ReadSignal<im::Vector<PaletteItem>>,
    pub input: RwSignal<PaletteInput>,
    kind: RwSignal<PaletteKind>,
    pub input_editor: EditorData,
    pub preview_editor: EditorData,
    pub has_preview: RwSignal<bool>,
    /// Listened on for which entry in the palette has been clicked
    pub clicked_index: RwSignal<Option<usize>>,
    pub main_split: MainSplitData,
    pub references: RwSignal<Vec<EditorLocation>>,
    pub common: Rc<CommonData>,
}

impl std::fmt::Debug for PaletteData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaletteData").finish()
    }
}

impl PaletteData {
    pub fn new(
        cx: Scope,
        workspace: Arc<LapceWorkspace>,
        main_split: MainSplitData,
        common: Rc<CommonData>,
    ) -> Self {
        let status = cx.create_rw_signal(PaletteStatus::Inactive);
        let items = cx.create_rw_signal(im::Vector::new());
        let preselect_index = cx.create_rw_signal(None);
        let index = cx.create_rw_signal(0);
        let references = cx.create_rw_signal(Vec::new());
        let input = cx.create_rw_signal(PaletteInput {
            input: "".to_string(),
            kind: PaletteKind::File,
        });
        let kind = cx.create_rw_signal(PaletteKind::File);
        let input_editor = main_split.editors.make_local(cx, common.clone());
        let preview_editor = main_split.editors.make_local(cx, common.clone());
        preview_editor.kind.set(EditorViewKind::Preview);
        let has_preview = cx.create_rw_signal(false);
        let run_id = cx.create_rw_signal(0);
        let run_id_counter = Arc::new(AtomicU64::new(0));

        // Two reactive effects feed a background thread that performs fuzzy filtering.
        // One triggers on items changes (new data loaded), the other on input changes (user typing).
        // They both send work to the same channel so the filter thread can batch/coalesce requests.
        let (run_tx, run_rx) = channel();
        {
            let run_id = run_id.read_only();
            let input = input.read_only();
            let items = items.read_only();
            let tx = run_tx;
            {
                let tx = tx.clone();
                // This effect triggers when items change (e.g., file list loaded from proxy).
                // It uses get_untracked for input/run_id to avoid subscribing to those signals.
                cx.create_effect(move |_| {
                    let items = items.get();
                    let input = input.get_untracked();
                    let run_id = run_id.get_untracked();
                    let preselect_index =
                        preselect_index.try_update(|i| i.take()).unwrap();
                    if let Err(err) =
                        tx.send((run_id, input.input, items, preselect_index))
                    {
                        tracing::error!("{:?}", err);
                    }
                });
            }
            // This effect triggers when the user types in the palette input.
            // It tracks the palette kind so that when the kind changes (e.g., switching
            // from file to line mode), it skips sending a filter request -- the kind
            // change will be handled by run_inner() which reloads items entirely.
            cx.create_effect(move |last_kind| {
                let input = input.get();
                let kind = input.kind;
                if last_kind != Some(kind) {
                    return kind;
                }
                let items = items.get_untracked();
                let run_id = run_id.get_untracked();
                if let Err(err) = tx.send((run_id, input.input, items, None)) {
                    tracing::error!("{:?}", err);
                }
                kind
            });
        }
        // Spawn a dedicated background thread for fuzzy filtering. This keeps the UI
        // responsive even with large item lists (e.g., thousands of files in a workspace).
        // The thread receives (run_id, input, items, preselect_index) tuples, coalesces
        // rapid updates, runs nucleo matching, and sends filtered results back.
        let (resp_tx, resp_rx) = channel();
        {
            let run_id = run_id_counter.clone();
            std::thread::Builder::new()
                .name("PaletteUpdateProcess".to_owned())
                .spawn(move || {
                    Self::update_process(run_id, run_rx, resp_tx);
                })
                .unwrap();
        }
        // Receive filtered results from the background thread. We validate that the
        // run_id and input still match the current state before applying, which prevents
        // stale results from overwriting newer ones (race condition protection).
        let (filtered_items, set_filtered_items) =
            cx.create_signal(im::Vector::new());
        {
            let resp = create_signal_from_channel(resp_rx);
            let run_id = run_id.read_only();
            let input = input.read_only();
            cx.create_effect(move |_| {
                if let Some((
                    filter_run_id,
                    filter_input,
                    new_items,
                    preselect_index,
                )) = resp.get()
                {
                    if run_id.get_untracked() == filter_run_id
                        && input.get_untracked().input == filter_input
                    {
                        set_filtered_items.set(new_items);
                        let i = preselect_index.unwrap_or(0);
                        index.set(i);
                    }
                }
            });
        }

        let clicked_index = cx.create_rw_signal(Option::<usize>::None);

        let palette = Self {
            run_id_counter,
            main_split,
            run_id,
            workspace,
            status,
            index,
            preselect_index,
            items,
            filtered_items,
            input_editor,
            preview_editor,
            has_preview,
            input,
            kind,
            clicked_index,
            references,
            common,
        };

        {
            let palette = palette.clone();
            let clicked_index = clicked_index.read_only();
            let index = index.write_only();
            cx.create_effect(move |_| {
                if let Some(clicked_index) = clicked_index.get() {
                    index.set(clicked_index);
                    palette.select();
                }
            });
        }

        {
            let palette = palette.clone();
            let doc = palette.input_editor.doc();
            let input = palette.input;
            let status = palette.status.read_only();
            let preset_kind = palette.kind.read_only();
            // Monitors when the palette's input changes, so that it can update the stored input
            // and kind of palette.
            cx.create_effect(move |last_input| {
                // TODO(minor, perf): this could have perf issues if the user accidentally pasted a huge amount of text into the palette.
                let new_input = doc.buffer.with(|buffer| buffer.to_string());

                let status = status.get_untracked();
                if status == PaletteStatus::Inactive {
                    // If the status is inactive, we set the input to None,
                    // so that when we actually run the palette, the input
                    // can be compared with this None.
                    return None;
                }

                let last_input = last_input.flatten();

                // If the input is not equivalent to the current input, or not initialized, then we
                // need to update the information about the palette.
                let changed = last_input.as_deref() != Some(new_input.as_str());

                if changed {
                    let new_kind = input
                        .try_update(|input| {
                            let kind = input.kind;
                            input.update_input(
                                new_input.clone(),
                                preset_kind.get_untracked(),
                            );
                            if last_input.is_none() || kind != input.kind {
                                Some(input.kind)
                            } else {
                                None
                            }
                        })
                        .unwrap();
                    if let Some(new_kind) = new_kind {
                        palette.run_inner(new_kind);
                    }
                }
                Some(new_input)
            });
        }

        {
            let palette = palette.clone();
            cx.create_effect(move |_| {
                let _ = palette.index.get();
                palette.preview();
            });
        }

        {
            let palette = palette.clone();
            cx.create_effect(move |_| {
                let focus = palette.common.focus.get();
                if focus != Focus::Palette
                    && palette.status.get_untracked() != PaletteStatus::Inactive
                {
                    palette.cancel();
                }
            });
        }

        palette
    }

    /// Start and focus the palette for the given kind.
    pub fn run(&self, kind: PaletteKind) {
        self.common.focus.set(Focus::Palette);
        self.status.set(PaletteStatus::Started);
        self.kind.set(kind);
        self.input_editor.doc().reload(Rope::from(""), true);
        self.input_editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::caret(0)));
    }

    /// Get the placeholder text to use in the palette input field.
    pub fn placeholder_text(&self) -> &'static str {
        ""
    }

    /// Execute the internal behavior of the palette for the given kind. This ignores updating and
    /// focusing the palette input.
    fn run_inner(&self, kind: PaletteKind) {
        self.has_preview.set(false);

        let run_id = self.run_id_counter.fetch_add(1, Ordering::Relaxed) + 1;
        self.run_id.set(run_id);

        match kind {
            PaletteKind::File => {
                self.get_files();
            }
            PaletteKind::Line => {
                self.get_lines();
            }
            PaletteKind::Workspace => {
                self.get_workspaces();
            }
            PaletteKind::Reference => {
                self.get_references();
            }
            PaletteKind::Language => {
                self.get_languages();
            }
            PaletteKind::LineEnding => {
                self.get_line_endings();
            }
        }
    }

    /// Initialize the palette with the files in the current workspace.
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
                        PaletteItem {
                            content: PaletteItemContent::File { path, full_path },
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

    /// Initialize the palette with the lines in the current document.
    fn get_lines(&self) {
        let editor = self.main_split.active_editor.get_untracked();
        let doc = match editor {
            Some(editor) => editor.doc(),
            None => {
                return;
            }
        };

        let buffer = doc.buffer.get_untracked();
        let last_line_number = buffer.last_line() + 1;
        let last_line_number_len = last_line_number.to_string().len();
        let items = buffer
            .text()
            .lines(0..buffer.len())
            .enumerate()
            .map(|(i, l)| {
                let line_number = i + 1;
                let text = format!(
                    "{}{} {}",
                    line_number,
                    vec![" "; last_line_number_len - line_number.to_string().len()]
                        .join(""),
                    l
                );
                PaletteItem {
                    content: PaletteItemContent::Line {
                        line: i,
                        content: text.clone(),
                    },
                    filter_text: text,
                    score: 0,
                    indices: vec![],
                }
            })
            .collect();
        self.items.set(items);
    }

    /// Initialize the palette with all the available workspaces, local and remote.
    fn get_workspaces(&self) {
        let db: Arc<LapceDb> = use_context().unwrap();
        let workspaces = db.recent_workspaces().unwrap_or_default();

        let items = workspaces
            .into_iter()
            .filter_map(|w| {
                let filter_text = w.path.as_ref()?.to_str()?.to_string();
                Some(PaletteItem {
                    content: PaletteItemContent::Workspace { workspace: w },
                    filter_text,
                    score: 0,
                    indices: vec![],
                })
            })
            .collect();

        self.items.set(items);
    }

    /// Initialize the list of references in the file, from the current editor location.
    fn get_references(&self) {
        let items = self
            .references
            .get_untracked()
            .into_iter()
            .map(|l| {
                let full_path = l.path.clone();
                let mut path = l.path.clone();
                if let Some(workspace_path) = self.workspace.path.as_ref() {
                    path = path
                        .strip_prefix(workspace_path)
                        .unwrap_or(&full_path)
                        .to_path_buf();
                }
                let filter_text = path.to_str().unwrap_or("").to_string();
                PaletteItem {
                    content: PaletteItemContent::Reference { path, location: l },
                    filter_text,
                    score: 0,
                    indices: vec![],
                }
            })
            .collect();

        self.items.set(items);
    }

    fn get_languages(&self) {
        let langs = LapceLanguage::languages();
        let items = langs
            .iter()
            .map(|lang| PaletteItem {
                content: PaletteItemContent::Language {
                    name: lang.to_string(),
                },
                filter_text: lang.to_string(),
                score: 0,
                indices: Vec::new(),
            })
            .collect();
        if let Some(editor) = self.main_split.active_editor.get_untracked() {
            let doc = editor.doc();
            let language =
                doc.syntax().with_untracked(|syntax| syntax.language.name());
            self.preselect_matching(&items, language);
        }
        self.items.set(items);
    }

    fn get_line_endings(&self) {
        let items = [LineEnding::Lf, LineEnding::CrLf]
            .iter()
            .map(|l| PaletteItem {
                content: PaletteItemContent::LineEnding { kind: *l },
                filter_text: l.as_str().to_string(),
                score: 0,
                indices: Vec::new(),
            })
            .collect();
        if let Some(editor) = self.main_split.active_editor.get_untracked() {
            let doc = editor.doc();
            let line_ending = doc.line_ending();
            self.preselect_matching(&items, line_ending.as_str());
        }
        self.items.set(items);
    }

    fn preselect_matching(&self, items: &im::Vector<PaletteItem>, matching: &str) {
        let Some((idx, _)) = items
            .iter()
            .find_position(|item| item.filter_text == matching)
        else {
            return;
        };

        self.preselect_index.set(Some(idx));
    }

    fn select(&self) {
        let index = self.index.get_untracked();
        let items = self.filtered_items.get_untracked();
        self.close();
        if let Some(item) = items.get(index) {
            match &item.content {
                PaletteItemContent::File { full_path, .. } => {
                    self.common
                        .internal_command
                        .send(InternalCommand::OpenFile {
                            path: full_path.clone(),
                        });
                }
                PaletteItemContent::Line { line, .. } => {
                    let editor = self.main_split.active_editor.get_untracked();
                    let doc = match editor {
                        Some(editor) => editor.doc(),
                        None => {
                            return;
                        }
                    };
                    let path = doc
                        .content
                        .with_untracked(|content| content.path().cloned());
                    let path = match path {
                        Some(path) => path,
                        None => return,
                    };
                    self.common.internal_command.send(
                        InternalCommand::JumpToLocation {
                            location: EditorLocation {
                                path,
                                position: Some(EditorPosition::Line(*line)),
                                scroll_offset: None,

                                same_editor_tab: false,
                            },
                        },
                    );
                }
                PaletteItemContent::Workspace { workspace } => {
                    self.common.window_common.window_command.send(
                        WindowCommand::SetWorkspace {
                            workspace: workspace.clone(),
                        },
                    );
                }
                PaletteItemContent::Reference { location, .. } => {
                    self.common.internal_command.send(
                        InternalCommand::JumpToLocation {
                            location: location.clone(),
                        },
                    );
                }
                PaletteItemContent::Language { name } => {
                    let editor = self.main_split.active_editor.get_untracked();
                    let doc = match editor {
                        Some(editor) => editor.doc(),
                        None => {
                            return;
                        }
                    };
                    if name.is_empty() || name.to_lowercase().eq("plain text") {
                        doc.set_syntax(Syntax::plaintext())
                    } else {
                        let lang = match LapceLanguage::from_name(name) {
                            Some(v) => v,
                            None => return,
                        };
                        doc.set_language(lang);
                    }
                    doc.trigger_syntax_change(None);
                }
                PaletteItemContent::LineEnding { kind } => {
                    let Some(editor) = self.main_split.active_editor.get_untracked()
                    else {
                        return;
                    };
                    let doc = editor.doc();

                    doc.buffer.update(|buffer| {
                        buffer.set_line_ending(*kind);
                    });
                }
            }
        }
    }

    /// Update the preview for the currently active palette item, if it has one.
    fn preview(&self) {
        if self.status.get_untracked() == PaletteStatus::Inactive {
            return;
        }

        let index = self.index.get_untracked();
        let items = self.filtered_items.get_untracked();
        if let Some(item) = items.get(index) {
            match &item.content {
                PaletteItemContent::File { .. } => {}
                PaletteItemContent::Line { line, .. } => {
                    self.has_preview.set(true);
                    let editor = self.main_split.active_editor.get_untracked();
                    let doc = match editor {
                        Some(editor) => editor.doc(),
                        None => {
                            return;
                        }
                    };
                    let path = doc
                        .content
                        .with_untracked(|content| content.path().cloned());
                    let path = match path {
                        Some(path) => path,
                        None => return,
                    };
                    self.preview_editor.update_doc(doc);
                    self.preview_editor.go_to_location(
                        EditorLocation {
                            path,
                            position: Some(EditorPosition::Line(*line)),
                            scroll_offset: None,

                            same_editor_tab: false,
                        },
                        false,
                        None,
                    );
                }
                PaletteItemContent::Workspace { .. } => {}
                PaletteItemContent::Language { .. } => {}
                PaletteItemContent::LineEnding { .. } => {}
                PaletteItemContent::Reference { location, .. } => {
                    self.has_preview.set(true);
                    let (doc, new_doc) =
                        self.main_split.get_doc(location.path.clone(), None);
                    self.preview_editor.update_doc(doc);
                    self.preview_editor.go_to_location(
                        location.clone(),
                        new_doc,
                        None,
                    );
                }
            }
        }
    }

    /// Cancel the palette, doing cleanup specific to the palette kind.
    /// For theme pickers, canceling must revert the live preview back to the saved config,
    /// which is why we reload the full config here.
    fn cancel(&self) {
        self.close();
    }

    /// Close the palette, reverting focus back to the workbench.
    fn close(&self) {
        self.status.set(PaletteStatus::Inactive);
        if self.common.focus.get_untracked() == Focus::Palette {
            self.common.focus.set(Focus::Workbench);
        }
        self.has_preview.set(false);
        self.items.update(|items| items.clear());
        self.input_editor.doc().reload(Rope::from(""), true);
        self.input_editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::caret(0)));
    }

    /// Move to the next entry in the palette list, wrapping around if needed.
    fn next(&self) {
        let index = self.index.get_untracked();
        let len = self.filtered_items.with_untracked(|i| i.len());
        let new_index = Movement::Down.update_index(index, len, 1, true);
        self.index.set(new_index);
    }

    /// Move to the previous entry in the palette list, wrapping around if needed.
    fn previous(&self) {
        let index = self.index.get_untracked();
        let len = self.filtered_items.with_untracked(|i| i.len());
        let new_index = Movement::Up.update_index(index, len, 1, true);
        self.index.set(new_index);
    }

    fn next_page(&self) {
        // TODO: implement
    }

    fn previous_page(&self) {
        // TODO: implement
    }

    fn run_focus_command(&self, cmd: &FocusCommand) -> CommandExecuted {
        match cmd {
            FocusCommand::ModalClose => {
                self.cancel();
            }
            FocusCommand::ListNext => {
                self.next();
            }
            FocusCommand::ListNextPage => {
                self.next_page();
            }
            FocusCommand::ListPrevious => {
                self.previous();
            }
            FocusCommand::ListPreviousPage => {
                self.previous_page();
            }
            FocusCommand::ListSelect => {
                self.select();
            }
            _ => return CommandExecuted::No,
        }
        CommandExecuted::Yes
    }

    /// Perform fuzzy filtering of palette items on the background thread.
    /// Returns `None` if the run_id changed mid-filter (meaning a newer request superseded this one),
    /// allowing the thread to skip sending stale results.
    fn filter_items(
        run_id: Arc<AtomicU64>,
        current_run_id: u64,
        input: &str,
        items: im::Vector<PaletteItem>,
        matcher: &mut nucleo::Matcher,
    ) -> Option<im::Vector<PaletteItem>> {
        // Empty input means show all items unfiltered (no fuzzy matching needed).
        if input.is_empty() {
            return Some(items);
        }

        let pattern = nucleo::pattern::Pattern::parse(
            input,
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Smart,
        );

        // NOTE: We collect into a Vec to sort as we are hitting a worst-case behavior in
        // `im::Vector` that can lead to a stack overflow!
        let mut filtered_items = Vec::new();
        let mut indices = Vec::new();
        let mut filter_text_buf = Vec::new();
        for i in &items {
            // If the run id has ever changed, then we'll just bail out of this filtering to avoid
            // wasting effort. This would happen, for example, on the user continuing to type.
            if run_id.load(std::sync::atomic::Ordering::Acquire) != current_run_id {
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

        if run_id.load(std::sync::atomic::Ordering::Acquire) != current_run_id {
            return None;
        }
        Some(filtered_items.into())
    }

    /// Background thread loop that receives filter requests, coalesces rapid updates
    /// (draining the channel to take only the latest), runs fuzzy filtering, and sends
    /// results back. The batch-receive pattern prevents the filter from running on every
    /// keystroke when the user types quickly.
    fn update_process(
        run_id: Arc<AtomicU64>,
        receiver: Receiver<(u64, String, im::Vector<PaletteItem>, Option<usize>)>,
        resp_tx: Sender<(u64, String, im::Vector<PaletteItem>, Option<usize>)>,
    ) {
        /// Drain all pending messages from the channel, keeping only the latest one.
        /// This coalesces rapid-fire updates so the filter only runs once.
        fn receive_batch(
            receiver: &Receiver<(
                u64,
                String,
                im::Vector<PaletteItem>,
                Option<usize>,
            )>,
        ) -> Result<(u64, String, im::Vector<PaletteItem>, Option<usize>)> {
            let (mut run_id, mut input, mut items, mut preselect_index) =
                receiver.recv()?;
            loop {
                match receiver.try_recv() {
                    Ok(update) => {
                        run_id = update.0;
                        input = update.1;
                        items = update.2;
                        preselect_index = update.3;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            Ok((run_id, input, items, preselect_index))
        }

        let mut matcher =
            nucleo::Matcher::new(nucleo::Config::DEFAULT.match_paths());
        loop {
            if let Ok((current_run_id, input, items, preselect_index)) =
                receive_batch(&receiver)
            {
                if let Some(filtered_items) = Self::filter_items(
                    run_id.clone(),
                    current_run_id,
                    &input,
                    items,
                    &mut matcher,
                ) {
                    if let Err(err) = resp_tx.send((
                        current_run_id,
                        input,
                        filtered_items,
                        preselect_index,
                    )) {
                        tracing::error!("{:?}", err);
                    }
                }
            } else {
                return;
            }
        }
    }
}

impl KeyPressFocus for PaletteData {
    fn check_condition(
        &self,
        condition: crate::keypress::condition::Condition,
    ) -> bool {
        matches!(
            condition,
            Condition::ListFocus | Condition::PaletteFocus | Condition::ModalFocus
        )
    }

    fn run_command(
        &self,
        command: &crate::command::LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Workbench(_) => {}
            CommandKind::Scroll(_) => {}
            CommandKind::Focus(cmd) => {
                self.run_focus_command(cmd);
            }
            CommandKind::Edit(_)
            | CommandKind::Move(_)
            | CommandKind::MultiSelection(_) => {
                self.input_editor.run_command(command, count, mods);
            }
            CommandKind::MotionMode(_) => {}
        }
        CommandExecuted::Yes
    }

    fn receive_char(&self, c: &str) {
        self.input_editor.receive_char(c);
    }
}
