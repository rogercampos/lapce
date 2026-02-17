use std::{ops::Range, path::PathBuf, rc::Rc, sync::Arc};

use floem::{
    View,
    event::EventListener,
    keyboard::Modifiers,
    peniko::{
        Color,
        kurbo::{Point, Size},
    },
    reactive::{
        Memo, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        create_rw_signal,
    },
    style::{CursorStyle, Display},
    views::{
        Decorators, VirtualVector, container, label, scroll,
        scroll::PropagatePointerWheel, stack, text, virtual_stack,
    },
};
use lapce_core::{
    buffer::Buffer, command::FocusCommand, mode::Mode, selection::Selection,
};
use lapce_rpc::proxy::SearchMatch;
use lapce_xi_rope::{Rope, find::CaseMatching};
use regex::Regex;

use crate::{
    about::exclusive_popup,
    command::{
        CommandExecuted, CommandKind, InternalCommand, LapceCommand,
        LapceWorkbenchCommand,
    },
    config::{LapceConfig, color::LapceColor, layout::LapceLayout},
    editor::view::editor_container_view,
    editor::{
        EditorData, EditorViewKind,
        location::{EditorLocation, EditorPosition},
    },
    focus_text::focus_text_highlighted,
    global_search::GlobalSearchData,
    keypress::KeyPressFocus,
    main_split::MainSplitData,
    resizable_container::resizable_container,
    search_modal::FlatSearchMatch,
    text_input::TextInputBuilder,
    workspace_data::{CommonData, Focus, WorkspaceData},
};

#[derive(Clone)]
pub struct ReplaceModalData {
    pub visible: RwSignal<bool>,
    pub index: RwSignal<usize>,
    pub search_editor: EditorData,
    pub replace_editor: EditorData,
    pub preview_editor: EditorData,
    pub has_preview: RwSignal<bool>,
    pub flat_matches: Memo<Vec<FlatSearchMatch>>,
    pub global_search: GlobalSearchData,
    pub main_split: MainSplitData,
    pub common: Rc<CommonData>,
    pub preview_focused: RwSignal<bool>,
    pub replace_input_focused: RwSignal<bool>,
}

impl std::fmt::Debug for ReplaceModalData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplaceModalData").finish()
    }
}

impl ReplaceModalData {
    pub fn new(
        cx: Scope,
        main_split: MainSplitData,
        global_search: GlobalSearchData,
        common: Rc<CommonData>,
    ) -> Self {
        let visible = cx.create_rw_signal(false);
        let index = cx.create_rw_signal(0usize);
        let search_editor = main_split.editors.make_local(cx, common.clone());
        let replace_editor = main_split.editors.make_local(cx, common.clone());
        let preview_editor = main_split.editors.make_local(cx, common.clone());
        preview_editor.kind.set(EditorViewKind::Preview);
        let has_preview = cx.create_rw_signal(false);

        // Sync search_editor buffer → global_search.set_pattern()
        {
            let global_search = global_search.clone();
            let buffer = search_editor.doc().buffer;
            cx.create_effect(move |_| {
                let content = buffer.with(|b| b.to_string());
                global_search.set_pattern(content);
            });
        }

        // Create flat_matches memo from grouped search results
        let search_result = global_search.search_result;
        let flat_matches = cx.create_memo(move |_| {
            search_result.with(|results| {
                results
                    .iter()
                    .flat_map(|(path, match_data)| {
                        match_data.matches.get().into_iter().map({
                            let path = path.clone();
                            move |m| FlatSearchMatch {
                                path: path.clone(),
                                search_match: m,
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            })
        });

        // Reset index and auto-preview first match when the search pattern changes.
        // We track the search editor buffer (not flat_matches) so that single-match
        // removals during replacement don't reset the index to 0.
        {
            let preview_editor = preview_editor.clone();
            let main_split = main_split.clone();
            let buffer = search_editor.doc().buffer;
            cx.create_effect(move |_| {
                // Track the search buffer — when the pattern changes, new results
                // arrive and we should reset to the first match.
                let _pattern = buffer.with(|b| b.to_string());
                // Read the current flat matches (untracked — we only want to
                // rerun when the pattern changes, not on every match removal).
                let matches = flat_matches.get_untracked();
                index.set(0);
                if let Some(m) = matches.first() {
                    let (doc, new_doc) = main_split.get_doc(m.path.clone(), None);
                    preview_editor.update_doc(doc);
                    preview_editor.go_to_location(
                        EditorLocation {
                            path: m.path.clone(),
                            position: Some(EditorPosition::Line(
                                m.search_match.line.saturating_sub(1),
                            )),
                            scroll_offset: None,
                            same_editor_tab: false,
                        },
                        new_doc,
                        None,
                    );
                    has_preview.set(true);
                } else {
                    has_preview.set(false);
                }
            });
        }

        // Auto-close when focus changes away from ReplaceModal
        {
            let focus = common.focus;
            let modal_active = global_search.modal_active;
            cx.create_effect(move |_| {
                let f = focus.get();
                if f != Focus::ReplaceModal && visible.get_untracked() {
                    modal_active.set(false);
                    visible.set(false);
                }
            });
        }

        let preview_focused = cx.create_rw_signal(false);
        let replace_input_focused = cx.create_rw_signal(false);

        Self {
            visible,
            index,
            search_editor,
            replace_editor,
            preview_editor,
            has_preview,
            flat_matches,
            global_search,
            main_split,
            common,
            preview_focused,
            replace_input_focused,
        }
    }

    pub fn open(&self) {
        self.search_editor.doc().reload(Rope::from(""), true);
        self.search_editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::caret(0)));
        self.replace_editor.doc().reload(Rope::from(""), true);
        self.replace_editor
            .cursor()
            .update(|cursor| cursor.set_insert(Selection::caret(0)));
        self.index.set(0);
        self.has_preview.set(false);
        self.preview_focused.set(false);
        self.replace_input_focused.set(false);

        // Grab word at cursor from active editor
        if self.common.focus.get_untracked() == Focus::Workbench {
            let active_editor = self.main_split.active_editor.get_untracked();
            if let Some(word) = active_editor.map(|editor| editor.word_at_cursor()) {
                if !word.is_empty() {
                    let word_len = word.len();
                    self.search_editor.doc().reload(Rope::from(&word), true);
                    self.search_editor.cursor().update(|cursor| {
                        cursor.set_insert(Selection::region(0, word_len))
                    });
                }
            }
        }

        self.global_search.modal_active.set(true);
        self.visible.set(true);
        self.common.focus.set(Focus::ReplaceModal);
    }

    pub fn close(&self) {
        self.global_search.modal_active.set(false);
        self.visible.set(false);
        if self.common.focus.get_untracked() == Focus::ReplaceModal {
            self.common.focus.set(Focus::Workbench);
        }
    }

    pub fn select(&self) {
        let matches = self.flat_matches.get_untracked();
        let idx = self.index.get_untracked();
        if let Some(m) = matches.get(idx) {
            self.common
                .internal_command
                .send(InternalCommand::JumpToLocation {
                    location: EditorLocation {
                        path: m.path.clone(),
                        position: Some(EditorPosition::Line(
                            m.search_match.line.saturating_sub(1),
                        )),
                        scroll_offset: None,
                        same_editor_tab: false,
                    },
                });
        }
        self.close();
    }

    fn next(&self) {
        self.preview_focused.set(false);
        let len = self.flat_matches.with_untracked(|items| items.len());
        if len == 0 {
            return;
        }
        let index = self.index.get_untracked();
        if index + 1 < len {
            self.index.set(index + 1);
            self.preview_match(index + 1);
        }
    }

    fn previous(&self) {
        self.preview_focused.set(false);
        let index = self.index.get_untracked();
        if index > 0 {
            self.index.set(index - 1);
            self.preview_match(index - 1);
        }
    }

    pub fn preview_match(&self, idx: usize) {
        let matches = self.flat_matches.get_untracked();
        if let Some(m) = matches.get(idx) {
            let (doc, new_doc) = self.main_split.get_doc(m.path.clone(), None);
            self.preview_editor.update_doc(doc);
            self.preview_editor.go_to_location(
                EditorLocation {
                    path: m.path.clone(),
                    position: Some(EditorPosition::Line(
                        m.search_match.line.saturating_sub(1),
                    )),
                    scroll_offset: None,
                    same_editor_tab: false,
                },
                new_doc,
                None,
            );
            self.has_preview.set(true);
        }
    }

    /// Replace the currently selected match.
    pub fn replace_single(&self) {
        let matches = self.flat_matches.get_untracked();
        let idx = self.index.get_untracked();
        let Some(m) = matches.get(idx) else { return };

        let replacement = self
            .replace_editor
            .doc()
            .buffer
            .with_untracked(|b| b.to_string());
        let pattern = self
            .search_editor
            .doc()
            .buffer
            .with_untracked(|b| b.to_string());
        if pattern.is_empty() {
            return;
        }

        let path = m.path.clone();
        let search_match = m.search_match.clone();
        let target_line = search_match.line; // 1-based

        // Read file from disk
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };

        let mut lines: Vec<String> =
            content.split('\n').map(|s| s.to_string()).collect();
        let line_idx = target_line.saturating_sub(1);
        if line_idx >= lines.len() {
            return;
        }

        let case_matching = self.global_search.case_matching.get_untracked();
        let whole_words = self.global_search.whole_words.get_untracked();
        let is_regex = self.global_search.is_regex.get_untracked();

        // Re-find the pattern on the actual line to get correct byte offsets
        let line = &lines[line_idx];
        let Some((byte_start, byte_end)) = find_pattern_on_line(
            line,
            &pattern,
            case_matching,
            whole_words,
            is_regex,
        ) else {
            return;
        };

        // Apply replacement
        let mut new_line = String::with_capacity(
            line.len() - (byte_end - byte_start) + replacement.len(),
        );
        new_line.push_str(&line[..byte_start]);
        new_line.push_str(&replacement);
        new_line.push_str(&line[byte_end..]);
        lines[line_idx] = new_line;

        let new_content = lines.join("\n");
        if std::fs::write(&path, &new_content).is_err() {
            return;
        }

        // Drop the borrowed `matches` Vec before mutating signals
        drop(matches);

        // Reload open doc if any
        self.reload_open_doc(&path, &new_content);

        // Remove the match from search results and update index/preview
        self.remove_match_and_advance(idx, &path, &search_match);
    }

    /// Replace all matches, then close the modal.
    pub fn replace_all(&self) {
        let matches = self.flat_matches.get_untracked();
        if matches.is_empty() {
            return;
        }

        let replacement = self
            .replace_editor
            .doc()
            .buffer
            .with_untracked(|b| b.to_string());
        let pattern = self
            .search_editor
            .doc()
            .buffer
            .with_untracked(|b| b.to_string());
        if pattern.is_empty() {
            return;
        }

        let case_matching = self.global_search.case_matching.get_untracked();
        let whole_words = self.global_search.whole_words.get_untracked();
        let is_regex = self.global_search.is_regex.get_untracked();

        // Group matches by file path — collect owned data to avoid borrow conflicts
        let mut by_file: indexmap::IndexMap<PathBuf, Vec<SearchMatch>> =
            indexmap::IndexMap::new();
        for m in matches.iter() {
            by_file
                .entry(m.path.clone())
                .or_default()
                .push(m.search_match.clone());
        }

        // Drop the borrowed matches before we start mutating
        drop(matches);

        // Collect all paths that need doc reload
        let mut changed_files: Vec<(PathBuf, String)> = Vec::new();

        for (path, file_matches) in &by_file {
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };

            let mut lines: Vec<String> =
                content.split('\n').map(|s| s.to_string()).collect();

            // Group by line, then process in reverse line order to preserve offsets
            let mut by_line: std::collections::BTreeMap<usize, Vec<&SearchMatch>> =
                std::collections::BTreeMap::new();
            for sm in file_matches {
                by_line.entry(sm.line).or_default().push(sm);
            }

            for (line_num, _line_matches) in by_line.iter().rev() {
                let line_idx = line_num.saturating_sub(1);
                if line_idx >= lines.len() {
                    continue;
                }
                // Replace all occurrences of the pattern on this line
                let line = &lines[line_idx];
                let new_line = replace_all_on_line(
                    line,
                    &pattern,
                    &replacement,
                    case_matching,
                    whole_words,
                    is_regex,
                );
                lines[line_idx] = new_line;
            }

            let new_content = lines.join("\n");
            if std::fs::write(path, &new_content).is_err() {
                continue;
            }
            changed_files.push((path.clone(), new_content));
        }

        // Reload open docs after all file writes are done
        for (path, new_content) in &changed_files {
            self.reload_open_doc(path, new_content);
        }

        // Clear search results
        self.global_search
            .search_result
            .set(indexmap::IndexMap::new());
        self.close();
    }

    fn reload_open_doc(&self, path: &PathBuf, new_content: &str) {
        let doc = self
            .main_split
            .docs
            .with_untracked(|docs| docs.get(path).cloned());
        if let Some(doc) = doc {
            doc.handle_file_changed(Rope::from(new_content));
        }
    }

    /// Remove a match from the search results and advance the preview to the
    /// next match. Signal updates are done outside the search_result borrow.
    fn remove_match_and_advance(
        &self,
        old_idx: usize,
        path: &PathBuf,
        search_match: &SearchMatch,
    ) {
        // First, update the inner matches signal for this file
        let is_file_empty =
            self.global_search.search_result.with_untracked(|results| {
                if let Some(match_data) = results.get(path) {
                    match_data.matches.update(|matches| {
                        if let Some(pos) = matches.iter().position(|m| {
                            m.line == search_match.line
                                && m.start == search_match.start
                                && m.end == search_match.end
                        }) {
                            matches.remove(pos);
                        }
                    });
                    match_data.matches.with_untracked(|m| m.is_empty())
                } else {
                    false
                }
            });

        // If the file has no more matches, remove it from the result map
        if is_file_empty {
            self.global_search.search_result.update(|results| {
                results.swap_remove(path);
            });
        }

        // Now read the updated flat matches and adjust index/preview
        let len = self.flat_matches.get_untracked().len();
        if len == 0 {
            self.has_preview.set(false);
            return;
        }
        let new_idx = old_idx.min(len.saturating_sub(1));
        self.index.set(new_idx);
        self.preview_match(new_idx);
    }
}

/// Find the first occurrence of the search pattern on a line, returning byte offsets.
fn find_pattern_on_line(
    line: &str,
    pattern: &str,
    case_matching: CaseMatching,
    whole_words: bool,
    is_regex: bool,
) -> Option<(usize, usize)> {
    if is_regex {
        let case_flag = match case_matching {
            CaseMatching::Exact => "",
            CaseMatching::CaseInsensitive => "(?i)",
        };
        let full_pattern = if whole_words {
            format!("{case_flag}\\b{pattern}\\b")
        } else {
            format!("{case_flag}{pattern}")
        };
        let re = Regex::new(&full_pattern).ok()?;
        let m = re.find(line)?;
        Some((m.start(), m.end()))
    } else {
        let needle = pattern;
        let haystack = line;

        let find_pos = match case_matching {
            CaseMatching::Exact => haystack.find(needle),
            CaseMatching::CaseInsensitive => {
                let lower_haystack = haystack.to_lowercase();
                let lower_needle = needle.to_lowercase();
                lower_haystack.find(&lower_needle)
            }
        };

        let start = find_pos?;
        let end = start + needle.len();

        if whole_words {
            let before_ok = start == 0
                || !haystack.as_bytes()[start - 1].is_ascii_alphanumeric()
                    && haystack.as_bytes()[start - 1] != b'_';
            let after_ok = end >= haystack.len()
                || !haystack.as_bytes()[end].is_ascii_alphanumeric()
                    && haystack.as_bytes()[end] != b'_';
            if before_ok && after_ok {
                Some((start, end))
            } else {
                None
            }
        } else {
            Some((start, end))
        }
    }
}

/// Replace all occurrences of a pattern on a single line.
fn replace_all_on_line(
    line: &str,
    pattern: &str,
    replacement: &str,
    case_matching: CaseMatching,
    whole_words: bool,
    is_regex: bool,
) -> String {
    if is_regex {
        let case_flag = match case_matching {
            CaseMatching::Exact => "",
            CaseMatching::CaseInsensitive => "(?i)",
        };
        let full_pattern = if whole_words {
            format!("{case_flag}\\b{pattern}\\b")
        } else {
            format!("{case_flag}{pattern}")
        };
        if let Ok(re) = Regex::new(&full_pattern) {
            re.replace_all(line, replacement).to_string()
        } else {
            line.to_string()
        }
    } else {
        let mut result = String::new();
        let mut remaining = line;
        loop {
            let found = match case_matching {
                CaseMatching::Exact => remaining.find(pattern),
                CaseMatching::CaseInsensitive => {
                    let lower = remaining.to_lowercase();
                    let lower_pat = pattern.to_lowercase();
                    lower.find(&lower_pat)
                }
            };
            let Some(pos) = found else {
                result.push_str(remaining);
                break;
            };
            let end = pos + pattern.len();

            if whole_words {
                let abs_start = line.len() - remaining.len() + pos;
                let abs_end = abs_start + pattern.len();
                let before_ok = abs_start == 0
                    || !line.as_bytes()[abs_start - 1].is_ascii_alphanumeric()
                        && line.as_bytes()[abs_start - 1] != b'_';
                let after_ok = abs_end >= line.len()
                    || !line.as_bytes()[abs_end].is_ascii_alphanumeric()
                        && line.as_bytes()[abs_end] != b'_';
                if before_ok && after_ok {
                    result.push_str(&remaining[..pos]);
                    result.push_str(replacement);
                    remaining = &remaining[end..];
                } else {
                    // Not a whole word match; skip past it
                    result.push_str(&remaining[..end]);
                    remaining = &remaining[end..];
                }
            } else {
                result.push_str(&remaining[..pos]);
                result.push_str(replacement);
                remaining = &remaining[end..];
            }
        }
        result
    }
}

impl KeyPressFocus for ReplaceModalData {
    fn get_mode(&self) -> Mode {
        Mode::Insert
    }

    fn check_condition(
        &self,
        condition: crate::keypress::condition::Condition,
    ) -> bool {
        use crate::keypress::condition::Condition;
        if self.preview_focused.get_untracked() {
            condition == Condition::ModalFocus
                || self.preview_editor.check_condition(condition)
        } else if self.replace_input_focused.get_untracked() {
            matches!(
                condition,
                Condition::ReplaceFocus
                    | Condition::ListFocus
                    | Condition::ModalFocus
            )
        } else {
            matches!(
                condition,
                Condition::SearchFocus
                    | Condition::ListFocus
                    | Condition::ModalFocus
            )
        }
    }

    fn run_command(
        &self,
        command: &LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        match &command.kind {
            CommandKind::Focus(cmd) => match cmd {
                FocusCommand::ModalClose => {
                    if self.preview_focused.get_untracked() {
                        self.preview_focused.set(false);
                    } else {
                        self.close();
                    }
                }
                FocusCommand::ListNext => self.next(),
                FocusCommand::ListPrevious => self.previous(),
                FocusCommand::ListSelect => self.replace_single(),
                FocusCommand::FocusReplaceEditor => {
                    self.replace_input_focused.set(true);
                }
                FocusCommand::FocusFindEditor => {
                    self.replace_input_focused.set(false);
                }
                FocusCommand::Search | FocusCommand::ClearSearch => {
                    // Suppress find-related commands on preview editors
                }
                _ => {
                    if self.preview_focused.get_untracked() {
                        return self
                            .preview_editor
                            .run_command(command, count, mods);
                    }
                    return CommandExecuted::No;
                }
            },
            CommandKind::Workbench(cmd) => match cmd {
                LapceWorkbenchCommand::SearchModalOpenFullResults => {
                    self.replace_all();
                }
                _ => return CommandExecuted::No,
            },
            _ => {
                if self.preview_focused.get_untracked() {
                    return self.preview_editor.run_command(command, count, mods);
                }
                if self.replace_input_focused.get_untracked() {
                    self.replace_editor.run_command(command, count, mods);
                } else {
                    self.search_editor.run_command(command, count, mods);
                }
            }
        }
        CommandExecuted::Yes
    }

    fn receive_char(&self, c: &str) {
        if self.preview_focused.get_untracked() {
            self.preview_editor.receive_char(c);
        } else if self.replace_input_focused.get_untracked() {
            self.replace_editor.receive_char(c);
        } else {
            self.search_editor.receive_char(c);
        }
    }

    fn focus_only(&self) -> bool {
        true
    }
}

// -- View --

struct FlatSearchItems(Vec<FlatSearchMatch>);

impl VirtualVector<(usize, FlatSearchMatch)> for FlatSearchItems {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, FlatSearchMatch)> {
        let start = range.start.min(self.0.len());
        let end = range.end.min(self.0.len());
        self.0[start..end]
            .iter()
            .cloned()
            .enumerate()
            .map(move |(i, item)| (i + start, item))
    }
}

pub fn replace_modal_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.replace_modal_data.clone();
    let config = workspace_data.common.config;
    let visibility = data.visible;
    let close_data = data.clone();

    exclusive_popup(
        config,
        visibility,
        move || close_data.close(),
        move || replace_modal_content(workspace_data),
    )
    .debug_name("Replace Modal Popup")
}

fn replace_modal_content(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.replace_modal_data.clone();
    let config = workspace_data.common.config;
    let focus = workspace_data.common.focus;
    let index = data.index;
    let flat_matches = data.flat_matches;
    let has_preview = data.has_preview;
    let item_height = 26.0;
    let search_buffer = data.search_editor.doc().buffer;

    let content = stack((
        // Header: Search + Replace inputs
        replace_modal_inputs(data.clone(), config, focus),
        // Body: results list + preview
        replace_modal_body(
            workspace_data.clone(),
            data.clone(),
            config,
            index,
            flat_matches,
            has_preview,
            search_buffer,
            item_height,
        ),
        // Footer: Replace / Replace All buttons
        replace_modal_footer(data, config),
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

fn replace_modal_inputs(
    data: ReplaceModalData,
    config: ReadSignal<Arc<LapceConfig>>,
    focus: RwSignal<Focus>,
) -> impl View {
    let preview_focused = data.preview_focused;
    let replace_input_focused = data.replace_input_focused;

    let search_is_focused = move || {
        focus.get() == Focus::ReplaceModal
            && !preview_focused.get()
            && !replace_input_focused.get()
    };
    let search_input = TextInputBuilder::new()
        .is_focused(search_is_focused)
        .build_editor(data.search_editor.clone())
        .placeholder(|| "Search in files...".to_owned())
        .style(|s| s.width_full());

    let replace_is_focused = move || {
        focus.get() == Focus::ReplaceModal
            && !preview_focused.get()
            && replace_input_focused.get()
    };
    let replace_input = TextInputBuilder::new()
        .is_focused(replace_is_focused)
        .build_editor(data.replace_editor.clone())
        .placeholder(|| "Replace with...".to_owned())
        .style(|s| s.width_full());

    container(
        stack((
            container(search_input)
                .on_event_cont(EventListener::PointerDown, move |_| {
                    preview_focused.set(false);
                    replace_input_focused.set(false);
                })
                .style(move |s| {
                    let config = config.get();
                    s.width_full()
                        .height(30.0)
                        .items_center()
                        .border_bottom(1.0)
                        .border_color(config.color(LapceColor::LAPCE_BORDER))
                        .background(config.color(LapceColor::EDITOR_BACKGROUND))
                }),
            container(replace_input)
                .on_event_cont(EventListener::PointerDown, move |_| {
                    preview_focused.set(false);
                    replace_input_focused.set(true);
                })
                .style(move |s| {
                    let config = config.get();
                    s.width_full()
                        .height(30.0)
                        .items_center()
                        .border_bottom(1.0)
                        .border_color(config.color(LapceColor::LAPCE_BORDER))
                        .background(config.color(LapceColor::EDITOR_BACKGROUND))
                }),
        ))
        .style(|s| s.flex_col().width_full()),
    )
    .style(|s| s.padding_bottom(5.0))
}

fn replace_modal_body(
    workspace_data: Rc<WorkspaceData>,
    data: ReplaceModalData,
    config: ReadSignal<Arc<LapceConfig>>,
    index: RwSignal<usize>,
    flat_matches: Memo<Vec<FlatSearchMatch>>,
    has_preview: RwSignal<bool>,
    search_buffer: RwSignal<Buffer>,
    item_height: f64,
) -> impl View {
    stack((
        // When there are matches: show results list + preview
        stack((
            // Results list
            scroll({
                let data = data.clone();
                virtual_stack(
                    move || FlatSearchItems(flat_matches.get()),
                    move |(i, m)| {
                        (
                            *i,
                            m.path.clone(),
                            m.search_match.line,
                            m.search_match.start,
                            m.search_match.end,
                        )
                    },
                    move |(i, m)| {
                        let data = data.clone();
                        let double_click_data = data.clone();
                        let line_content = m.search_match.line_content.clone();
                        let line_number = m.search_match.line;
                        let start = m.search_match.start;
                        let end = m.search_match.end;
                        let filename = m
                            .path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let location_label = format!("{}:{}", filename, line_number);
                        let line_content_for_trim = line_content.clone();
                        let syntax_line_content =
                            m.search_match.line_content.clone();
                        let syntax_path = m.path.clone();
                        let main_split = data.main_split.clone();

                        container(
                            stack((
                                focus_text_highlighted(
                                    move || {
                                        let config = config.get();
                                        if config.ui.trim_search_results_whitespace {
                                            line_content.trim().to_string()
                                        } else {
                                            line_content.clone()
                                        }
                                    },
                                    move || {
                                        let config = config.get();
                                        let offset = if config
                                            .ui
                                            .trim_search_results_whitespace
                                        {
                                            line_content_for_trim.trim_start().len()
                                                as i32
                                                - line_content_for_trim.len() as i32
                                        } else {
                                            0
                                        };
                                        ((start as i32 + offset) as usize
                                            ..(end as i32 + offset) as usize)
                                            .collect()
                                    },
                                    move || {
                                        config.get().color(LapceColor::EDITOR_FOCUS)
                                    },
                                    move || {
                                        let config = config.get();
                                        let trim =
                                            config.ui.trim_search_results_whitespace;
                                        let trim_offset = if trim {
                                            syntax_line_content.len()
                                                - syntax_line_content
                                                    .trim_start()
                                                    .len()
                                        } else {
                                            0
                                        };

                                        let (doc, _new) = main_split
                                            .get_doc(syntax_path.clone(), None);
                                        let _rev = doc.cache_rev.get();
                                        let line_styles = doc.line_style(
                                            line_number.saturating_sub(1),
                                        );
                                        line_styles
                                            .iter()
                                            .filter_map(|ls| {
                                                let color = ls
                                                    .style
                                                    .fg_color
                                                    .as_ref()
                                                    .and_then(|name| {
                                                        config.style_color(name)
                                                    })?;
                                                let s = ls
                                                    .start
                                                    .saturating_sub(trim_offset);
                                                let e = ls
                                                    .end
                                                    .saturating_sub(trim_offset);
                                                if s < e {
                                                    Some((s, e, color))
                                                } else {
                                                    None
                                                }
                                            })
                                            .collect()
                                    },
                                    Color::BLACK,
                                    Color::from_rgb8(0xBB, 0xBB, 0x00),
                                    item_height,
                                )
                                .style(|s| s.min_width(0.0)),
                                container(text(""))
                                    .style(|s| s.flex_grow(1.0).min_width(10.0)),
                                label(move || location_label.clone()).style(
                                    move |s| {
                                        s.color(
                                            config
                                                .get()
                                                .color(LapceColor::EDITOR_DIM),
                                        )
                                        .flex_shrink(0.0)
                                    },
                                ),
                            ))
                            .style(|s| s.width_full().items_center()),
                        )
                        .on_click_stop(move |_| {
                            data.preview_focused.set(false);
                            data.index.set(i);
                            data.preview_match(i);
                        })
                        .on_double_click_stop(move |_| {
                            double_click_data.index.set(i);
                            double_click_data.select();
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
            .style(move |s| {
                s.width_full()
                    .min_height(0.0)
                    .flex_basis(0.0)
                    .flex_grow(1.0)
                    .set(PropagatePointerWheel, false)
            }),
            // Preview editor (50% of body)
            replace_modal_preview_editor(workspace_data, config),
        ))
        .style(move |s| {
            s.display(if has_preview.get() {
                Display::Flex
            } else {
                Display::None
            })
            .size_full()
            .flex_col()
        }),
        // When no matches: placeholder text
        container(
            label(move || {
                let input_text = search_buffer.with(|b| b.to_string());
                let is_empty = flat_matches.with(|items| items.is_empty());
                if input_text.is_empty() {
                    "Type search query to find in files".to_string()
                } else if is_empty {
                    "No results".to_string()
                } else {
                    String::new()
                }
            })
            .style(move |s| s.color(config.get().color(LapceColor::EDITOR_DIM))),
        )
        .style(move |s| {
            let config = config.get();
            s.display(if has_preview.get() {
                Display::None
            } else {
                Display::Flex
            })
            .size_full()
            .items_center()
            .justify_center()
            .border_top(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
    ))
    .style(|s| s.flex_grow(1.0).min_height(0.0))
}

fn replace_modal_preview_editor(
    workspace_data: Rc<WorkspaceData>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let data = workspace_data.replace_modal_data.clone();
    let preview_focused = data.preview_focused;
    let workspace = workspace_data.workspace.clone();
    let preview_editor = create_rw_signal(data.preview_editor.clone());

    container(
        container(editor_container_view(
            workspace_data,
            workspace,
            |_tracked: bool| true,
            preview_editor,
        ))
        .on_event_cont(EventListener::PointerDown, move |_| {
            preview_focused.set(true);
        })
        .style(move |s| {
            let config = config.get();
            s.position(floem::style::Position::Absolute)
                .border_top(1.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
                .size_full()
                .background(config.color(LapceColor::EDITOR_BACKGROUND))
        }),
    )
    .style(|s| s.flex_basis(0.0).flex_grow(1.0).min_height(0.0))
}

fn replace_modal_footer(
    data: ReplaceModalData,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let replace_data = data.clone();
    let replace_all_data = data.clone();

    let modifier = if cfg!(target_os = "macos") {
        "\u{2318}"
    } else {
        "Ctrl"
    };

    stack((
        // Replace button
        label(|| "Replace".to_string())
            .on_click_stop(move |_| {
                replace_data.replace_single();
            })
            .style(move |s| {
                let config = config.get();
                s.color(config.color(LapceColor::EDITOR_DIM))
                    .font_size(12.0)
                    .padding_horiz(10.0)
                    .padding_vert(4.0)
                    .border(1.0)
                    .border_radius(3.0)
                    .border_color(config.color(LapceColor::LAPCE_BORDER))
                    .cursor(CursorStyle::Pointer)
                    .hover(|s| {
                        s.background(
                            config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                        )
                    })
            }),
        label(|| "Enter".to_string()).style(move |s| {
            let config = config.get();
            s.color(config.color(LapceColor::EDITOR_DIM))
                .font_size(11.0)
                .padding_horiz(6.0)
                .padding_vert(2.0)
                .margin_left(4.0)
                .border(1.0)
                .border_radius(3.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
        }),
        container(text("")).style(|s| s.flex_grow(1.0)),
        // Replace All button
        label(|| "Replace All".to_string())
            .on_click_stop(move |_| {
                replace_all_data.replace_all();
            })
            .style(move |s| {
                let config = config.get();
                s.color(config.color(LapceColor::EDITOR_DIM))
                    .font_size(12.0)
                    .padding_horiz(10.0)
                    .padding_vert(4.0)
                    .border(1.0)
                    .border_radius(3.0)
                    .border_color(config.color(LapceColor::LAPCE_BORDER))
                    .cursor(CursorStyle::Pointer)
                    .hover(|s| {
                        s.background(
                            config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                        )
                    })
            }),
        label(move || format!("{modifier}+Enter")).style(move |s| {
            let config = config.get();
            s.color(config.color(LapceColor::EDITOR_DIM))
                .font_size(11.0)
                .padding_horiz(6.0)
                .padding_vert(2.0)
                .margin_left(4.0)
                .border(1.0)
                .border_radius(3.0)
                .border_color(config.color(LapceColor::LAPCE_BORDER))
        }),
    ))
    .style(move |s| {
        let config = config.get();
        s.width_full()
            .padding_horiz(12.0)
            .padding_vert(6.0)
            .border_top(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .items_center()
    })
}
