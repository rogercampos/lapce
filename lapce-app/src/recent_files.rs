use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use floem::{
    View,
    keyboard::Modifiers,
    peniko::kurbo::{Point, Size},
    reactive::{
        Memo, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        create_memo,
    },
    style::{CursorStyle, Display},
    views::{
        Decorators, VirtualVector, container, scroll, scroll::PropagatePointerWheel,
        stack, text, virtual_stack,
    },
};
use lapce_core::{command::FocusCommand, mode::Mode, selection::Selection};
use lapce_xi_rope::Rope;
use nucleo::Utf32Str;

use crate::{
    about::exclusive_popup,
    command::{CommandExecuted, CommandKind, LapceCommand},
    config::{LapceConfig, color::LapceColor, layout::LapceLayout},
    editor::EditorData,
    editor::location::EditorLocation,
    keypress::KeyPressFocus,
    main_split::MainSplitData,
    resizable_container::resizable_container,
    text_input::TextInputBuilder,
    workspace_data::{CommonData, Focus, WorkspaceData},
};

/// Data model for the recent files popup. Uses nucleo for fuzzy filtering
/// of the file list as the user types. The popup shares the exclusive_popup
/// pattern with the search modal and about dialog.
#[derive(Clone)]
pub struct RecentFilesData {
    pub visible: RwSignal<bool>,
    /// Currently highlighted item in the filtered list, controlled by up/down keys.
    pub index: RwSignal<usize>,
    pub input_editor: EditorData,
    /// The full unfiltered list of recently opened file paths.
    pub recent_files: RwSignal<Vec<PathBuf>>,
    /// Derived filtered list: when input is empty, shows all files; otherwise
    /// uses nucleo fuzzy matching on filenames and sorts by score descending.
    pub filtered_items: Memo<Vec<PathBuf>>,
    pub main_split: MainSplitData,
    pub common: Rc<CommonData>,
}

impl std::fmt::Debug for RecentFilesData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecentFilesData").finish()
    }
}

impl RecentFilesData {
    pub fn new(
        cx: Scope,
        main_split: MainSplitData,
        recent_files: RwSignal<Vec<PathBuf>>,
        common: Rc<CommonData>,
    ) -> Self {
        let visible = cx.create_rw_signal(false);
        let index = cx.create_rw_signal(0usize);
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

        // Fuzzy filtering using nucleo: this runs synchronously in the memo because
        // the recent files list is typically small (< 100 items). For larger lists
        // (like the file palette), a background thread would be needed.
        let filtered_items = cx.create_memo(move |_| {
            let files = recent_files.get();
            let input = filter_text.get();

            if input.is_empty() {
                return files;
            }

            let pattern = nucleo::pattern::Pattern::parse(
                &input,
                nucleo::pattern::CaseMatching::Ignore,
                nucleo::pattern::Normalization::Smart,
            );
            let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
            let mut buf = Vec::new();

            let mut scored: Vec<(PathBuf, u32)> = files
                .into_iter()
                .filter_map(|path| {
                    buf.clear();
                    let filename = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());
                    let utf32 = Utf32Str::new(&filename, &mut buf);
                    let score = pattern.score(utf32, &mut matcher)?;
                    Some((path, score))
                })
                .collect();

            scored.sort_by(|a, b| b.1.cmp(&a.1));
            scored.into_iter().map(|(p, _)| p).collect()
        });

        // Reset index when filtered items change
        {
            cx.create_effect(move |_| {
                let _ = filtered_items.get();
                index.set(0);
            });
        }

        // Auto-close when focus changes away
        {
            let visible = visible;
            let focus = common.focus;
            cx.create_effect(move |_| {
                let f = focus.get();
                if f != Focus::RecentFiles && visible.get_untracked() {
                    visible.set(false);
                }
            });
        }

        Self {
            visible,
            index,
            input_editor,
            recent_files,
            filtered_items,
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
        self.common.focus.set(Focus::RecentFiles);
    }

    pub fn close(&self) {
        self.visible.set(false);
        Focus::restore_if_matching(&self.common.focus, Focus::RecentFiles);
    }

    pub fn select(&self) {
        let items = self.filtered_items.get_untracked();
        let idx = self.index.get_untracked();
        if let Some(path) = items.get(idx) {
            self.main_split.go_to_location(
                EditorLocation {
                    path: path.clone(),
                    position: None,
                    scroll_offset: None,
                    same_editor_tab: false,
                },
                None,
            );
        }
        self.close();
    }

    fn next(&self) {
        let len = self.filtered_items.with_untracked(|items| items.len());
        if len == 0 {
            return;
        }
        let index = self.index.get_untracked();
        if index + 1 < len {
            self.index.set(index + 1);
        }
    }

    fn previous(&self) {
        let len = self.filtered_items.with_untracked(|items| items.len());
        if len == 0 {
            return;
        }
        let index = self.index.get_untracked();
        if index > 0 {
            self.index.set(index - 1);
        }
    }
}

impl KeyPressFocus for RecentFilesData {
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

struct RecentFileItems(Vec<PathBuf>);

impl VirtualVector<(usize, PathBuf)> for RecentFileItems {
    fn total_len(&self) -> usize {
        self.0.len()
    }

    fn slice(
        &mut self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, PathBuf)> {
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

/// Pre-computes the set of filenames that appear more than once in the list.
fn duplicate_filenames(items: &[PathBuf]) -> HashSet<OsString> {
    let mut counts: HashMap<OsString, u32> = HashMap::new();
    for path in items {
        if let Some(name) = path.file_name() {
            *counts.entry(name.to_os_string()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name)
        .collect()
}

/// Extracts the filename and an optional disambiguating directory hint for display.
/// The directory hint is only shown when multiple files share the same filename,
/// so the user can tell them apart. The path is stripped relative to the workspace
/// root when possible for brevity.
fn file_display_parts(
    path: &PathBuf,
    duplicates: &HashSet<OsString>,
    workspace_path: &Option<PathBuf>,
) -> (String, String) {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let is_duplicate = path
        .file_name()
        .map(|n| duplicates.contains(n))
        .unwrap_or(false);

    let dir_hint = if is_duplicate {
        path.parent()
            .and_then(|p| {
                workspace_path
                    .as_ref()
                    .and_then(|ws| p.strip_prefix(ws).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .or_else(|| Some(crate::path::display_path(p)))
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    (filename, dir_hint)
}

pub fn recent_files_popup(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.palettes.recent_files.clone();
    let config = workspace_data.common.config;
    let visibility = data.visible;
    let close_data = data.clone();

    exclusive_popup(
        config,
        visibility,
        move || close_data.close(),
        move || recent_files_content(workspace_data),
    )
    .debug_name("Recent Files Popup")
}

fn recent_files_content(workspace_data: Rc<WorkspaceData>) -> impl View {
    let data = workspace_data.palettes.recent_files.clone();
    let config = workspace_data.common.config;
    let focus = workspace_data.common.focus;
    let index = data.index;
    let filtered_items = data.filtered_items;
    let workspace_path = workspace_data.workspace.path.clone();
    let item_height = 30.0;
    let duplicates =
        create_memo(move |_| duplicate_filenames(&filtered_items.get()));

    let content = stack((
        recent_files_input(data.clone(), config, focus),
        scroll({
            let data = data.clone();
            let workspace_path = workspace_path.clone();
            virtual_stack(
                move || RecentFileItems(filtered_items.get()),
                move |(i, path)| (*i, path.clone()),
                move |(i, path)| {
                    let duplicates = duplicates.get_untracked();
                    let (filename, dir_hint) =
                        file_display_parts(&path, &duplicates, &workspace_path);
                    let data = data.clone();
                    let icon_path = path.clone();

                    container(crate::file_icon::file_icon_with_name(
                        config,
                        move || icon_path.clone(),
                        move || filename.clone(),
                        move || dir_hint.clone(),
                    ))
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
                                s.background(
                                    config.color(
                                        LapceColor::PALETTE_CURRENT_BACKGROUND,
                                    ),
                                )
                            })
                            .hover(|s| {
                                s.background(
                                    config
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
                .flex_grow(1.0)
                .set(PropagatePointerWheel, false)
        }),
        text("No recent files").style(move |s| {
            s.display(if filtered_items.with(|items| items.is_empty()) {
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

    resizable_container(500.0, 450.0, 300.0, 200.0, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── duplicate_filenames ──

    #[test]
    fn duplicate_filenames_empty_list() {
        let items: Vec<PathBuf> = vec![];
        assert!(duplicate_filenames(&items).is_empty());
    }

    #[test]
    fn duplicate_filenames_no_duplicates() {
        let items = vec![
            PathBuf::from("/a/foo.rs"),
            PathBuf::from("/b/bar.rs"),
            PathBuf::from("/c/baz.rs"),
        ];
        assert!(duplicate_filenames(&items).is_empty());
    }

    #[test]
    fn duplicate_filenames_with_duplicates() {
        let items = vec![
            PathBuf::from("/a/foo.rs"),
            PathBuf::from("/b/foo.rs"),
            PathBuf::from("/c/bar.rs"),
        ];
        let dups = duplicate_filenames(&items);
        assert_eq!(dups.len(), 1);
        assert!(dups.contains(&OsString::from("foo.rs")));
    }

    #[test]
    fn duplicate_filenames_multiple_duplicates() {
        let items = vec![
            PathBuf::from("/a/foo.rs"),
            PathBuf::from("/b/foo.rs"),
            PathBuf::from("/c/bar.rs"),
            PathBuf::from("/d/bar.rs"),
            PathBuf::from("/e/unique.rs"),
        ];
        let dups = duplicate_filenames(&items);
        assert_eq!(dups.len(), 2);
        assert!(dups.contains(&OsString::from("foo.rs")));
        assert!(dups.contains(&OsString::from("bar.rs")));
    }

    #[test]
    fn duplicate_filenames_triple_occurrence() {
        let items = vec![
            PathBuf::from("/a/mod.rs"),
            PathBuf::from("/b/mod.rs"),
            PathBuf::from("/c/mod.rs"),
        ];
        let dups = duplicate_filenames(&items);
        assert_eq!(dups.len(), 1);
        assert!(dups.contains(&OsString::from("mod.rs")));
    }

    #[test]
    fn duplicate_filenames_root_paths_no_file_name() {
        // PathBuf::from("/") has no file_name()
        let items = vec![PathBuf::from("/"), PathBuf::from("/")];
        assert!(duplicate_filenames(&items).is_empty());
    }

    // ── file_display_parts ──

    #[test]
    fn display_parts_no_duplicate_no_workspace() {
        let path = PathBuf::from("/home/user/project/src/main.rs");
        let dups = HashSet::new();
        let ws: Option<PathBuf> = None;
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        assert_eq!(filename, "main.rs");
        assert_eq!(dir_hint, "");
    }

    #[test]
    fn display_parts_duplicate_no_workspace() {
        let path = PathBuf::from("/home/user/project/src/main.rs");
        let dups = {
            let mut s = HashSet::new();
            s.insert(OsString::from("main.rs"));
            s
        };
        let ws: Option<PathBuf> = None;
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        assert_eq!(filename, "main.rs");
        // Without workspace, shows full parent path
        assert_eq!(dir_hint, "/home/user/project/src");
    }

    #[test]
    fn display_parts_duplicate_with_workspace() {
        let path = PathBuf::from("/home/user/project/src/main.rs");
        let dups = {
            let mut s = HashSet::new();
            s.insert(OsString::from("main.rs"));
            s
        };
        let ws = Some(PathBuf::from("/home/user/project"));
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        assert_eq!(filename, "main.rs");
        // With workspace, shows relative parent path
        assert_eq!(dir_hint, "src");
    }

    #[test]
    fn display_parts_duplicate_path_outside_workspace() {
        let path = PathBuf::from("/other/location/main.rs");
        let dups = {
            let mut s = HashSet::new();
            s.insert(OsString::from("main.rs"));
            s
        };
        let ws = Some(PathBuf::from("/home/user/project"));
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        assert_eq!(filename, "main.rs");
        // Outside workspace, falls back to full parent path
        assert_eq!(dir_hint, "/other/location");
    }

    #[test]
    fn display_parts_not_duplicate_with_workspace() {
        let path = PathBuf::from("/home/user/project/src/lib.rs");
        let dups = HashSet::new();
        let ws = Some(PathBuf::from("/home/user/project"));
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        assert_eq!(filename, "lib.rs");
        // Not a duplicate — no dir hint even with workspace
        assert_eq!(dir_hint, "");
    }

    #[test]
    fn display_parts_root_file() {
        // A file directly in root
        let path = PathBuf::from("/config.toml");
        let dups = {
            let mut s = HashSet::new();
            s.insert(OsString::from("config.toml"));
            s
        };
        let ws: Option<PathBuf> = None;
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        assert_eq!(filename, "config.toml");
        assert_eq!(dir_hint, "/");
    }

    #[test]
    fn display_parts_deep_nested_duplicate_with_workspace() {
        let path = PathBuf::from("/workspace/src/modules/feature/components/mod.rs");
        let dups = {
            let mut s = HashSet::new();
            s.insert(OsString::from("mod.rs"));
            s
        };
        let ws = Some(PathBuf::from("/workspace"));
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        assert_eq!(filename, "mod.rs");
        assert_eq!(dir_hint, "src/modules/feature/components");
    }

    #[test]
    fn display_parts_path_with_no_filename() {
        // PathBuf::from("/") has no file_name
        let path = PathBuf::from("/");
        let dups = HashSet::new();
        let ws: Option<PathBuf> = None;
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        // Falls back to the full path string
        assert_eq!(filename, "/");
        assert_eq!(dir_hint, "");
    }

    #[test]
    fn display_parts_workspace_is_parent_dir() {
        let path = PathBuf::from("/ws/main.rs");
        let dups = {
            let mut s = HashSet::new();
            s.insert(OsString::from("main.rs"));
            s
        };
        let ws = Some(PathBuf::from("/ws"));
        let (filename, dir_hint) = file_display_parts(&path, &dups, &ws);
        assert_eq!(filename, "main.rs");
        // Parent is /ws, stripped of workspace prefix → empty
        assert_eq!(dir_hint, "");
    }
}

fn recent_files_input(
    data: RecentFilesData,
    config: ReadSignal<Arc<LapceConfig>>,
    focus: RwSignal<Focus>,
) -> impl View {
    let is_focused = move || focus.get() == Focus::RecentFiles;
    let input = TextInputBuilder::new()
        .is_focused(is_focused)
        .build_editor(data.input_editor.clone())
        .placeholder(|| "Search recent files...".to_owned())
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
