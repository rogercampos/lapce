use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use floem::{
    peniko::{
        Color,
        kurbo::{Point, Rect},
    },
    reactive::{
        Memo, ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        create_memo,
    },
    views::editor::id::EditorId,
};
use serde::{Deserialize, Serialize};

use crate::{
    config::{LapceConfig, color::LapceColor, icon::LapceIcons},
    doc::{Doc, DocContent, is_external_file},
    editor::{EditorData, EditorInfo, location::EditorLocation},
    id::{EditorTabId, KeymapId, ProjectsId, SettingsId, SplitId},
    main_split::{Editors, MainSplitData},
    workspace_data::WorkspaceData,
};

/// Serializable snapshot of an editor tab child for persistence.
/// Used when saving workspace state to disk and restoring it on launch.
#[derive(Clone, Serialize, Deserialize)]
pub enum EditorTabChildInfo {
    Editor(EditorInfo),
    Settings,
    Keymap,
    Projects,
}

impl EditorTabChildInfo {
    pub fn to_data(
        &self,
        data: MainSplitData,
        editor_tab_id: EditorTabId,
    ) -> EditorTabChild {
        match &self {
            EditorTabChildInfo::Editor(editor_info) => {
                let editor_id = editor_info.to_data(data, editor_tab_id);
                EditorTabChild::Editor(editor_id)
            }
            EditorTabChildInfo::Settings => {
                EditorTabChild::Settings(SettingsId::next())
            }
            EditorTabChildInfo::Keymap => EditorTabChild::Keymap(KeymapId::next()),
            EditorTabChildInfo::Projects => {
                EditorTabChild::Projects(ProjectsId::next())
            }
        }
    }
}

/// Serializable snapshot of an entire editor tab pane for workspace persistence.
/// `is_focus` records which tab pane was focused when saved, so it can be restored.
#[derive(Clone, Serialize, Deserialize)]
pub struct EditorTabInfo {
    pub active: usize,
    pub is_focus: bool,
    pub children: Vec<EditorTabChildInfo>,
}

impl EditorTabInfo {
    pub fn to_data(
        &self,
        data: MainSplitData,
        split: SplitId,
    ) -> RwSignal<EditorTabData> {
        let editor_tab_id = EditorTabId::next();
        let editor_tab_data = {
            let cx = data.scope.create_child();
            let children_count = self.children.len();
            let editor_tab_data = EditorTabData {
                scope: cx,
                editor_tab_id,
                split,
                active: self.active.min(children_count.saturating_sub(1)),
                children: self
                    .children
                    .iter()
                    .map(|child| {
                        (
                            cx.create_rw_signal(0),
                            cx.create_rw_signal(Rect::ZERO),
                            child.to_data(data.clone(), editor_tab_id),
                        )
                    })
                    .collect(),
                layout_rect: Rect::ZERO,
                window_origin: Point::ZERO,
                locations: cx.create_rw_signal(im::Vector::new()),
                current_location: cx.create_rw_signal(0),
            };
            cx.create_rw_signal(editor_tab_data)
        };
        if self.is_focus {
            data.active_editor_tab.set(Some(editor_tab_id));
        }
        data.editor_tabs.update(|editor_tabs| {
            editor_tabs.insert(editor_tab_id, editor_tab_data);
        });
        editor_tab_data
    }
}

/// Describes what kind of content to open in a tab. Used as input to
/// `get_editor_tab_child()` to determine what to create or reuse.
/// `Editor` carries a pre-loaded Doc to avoid redundant file loading.
pub enum EditorTabChildSource {
    Editor { path: PathBuf, doc: Rc<Doc> },
    NewFileEditor,
    Settings,
    Keymap,
    Projects,
}

/// A live child within an editor tab pane. Each variant holds an ID for its specific
/// content type. Editor children reference an EditorId in the Editors registry;
/// special views (Settings, Keymap, etc.) have their own ID types to distinguish
/// multiple instances and enable deduplication in get_editor_tab_child().
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorTabChild {
    Editor(EditorId),
    Settings(SettingsId),
    Keymap(KeymapId),
    Projects(ProjectsId),
}

#[derive(PartialEq)]
pub struct EditorTabChildViewInfo {
    pub icon: String,
    pub color: Option<Color>,
    pub name: String,
    pub path: Option<PathBuf>,
    pub is_pristine: bool,
    pub is_external: bool,
}

impl EditorTabChild {
    pub fn id(&self) -> u64 {
        match self {
            EditorTabChild::Editor(id) => id.to_raw(),
            EditorTabChild::Settings(id) => id.to_raw(),
            EditorTabChild::Keymap(id) => id.to_raw(),
            EditorTabChild::Projects(id) => id.to_raw(),
        }
    }

    pub fn is_settings(&self) -> bool {
        matches!(self, EditorTabChild::Settings(_))
    }

    pub fn child_info(&self, data: &WorkspaceData) -> EditorTabChildInfo {
        match &self {
            EditorTabChild::Editor(editor_id) => {
                let editor_data = data
                    .main_split
                    .editors
                    .editor_untracked(*editor_id)
                    .unwrap();
                EditorTabChildInfo::Editor(editor_data.editor_info(data))
            }
            EditorTabChild::Settings(_) => EditorTabChildInfo::Settings,
            EditorTabChild::Keymap(_) => EditorTabChildInfo::Keymap,
            EditorTabChild::Projects(_) => EditorTabChildInfo::Projects,
        }
    }

    /// Creates a reactive Memo that computes the tab header display info (icon, name,
    /// color, pristine state) for this child. The Memo subscribes to the relevant
    /// signals so tab headers automatically update when files are renamed, modified,
    /// or when the config/theme changes.
    pub fn view_info(
        &self,
        editors: Editors,
        config: ReadSignal<Arc<LapceConfig>>,
        workspace_path: Option<PathBuf>,
    ) -> Memo<EditorTabChildViewInfo> {
        match self.clone() {
            EditorTabChild::Editor(editor_id) => create_memo(move |_| {
                let config = config.get();
                let editor_data = editors.editor(editor_id);
                let path = if let Some(editor_data) = editor_data {
                    let doc = editor_data.doc_signal().get();
                    let (content, is_pristine) =
                        (doc.content.get(), doc.buffer.with(|b| b.is_pristine()));
                    match content {
                        DocContent::File { path, .. } => Some((path, is_pristine)),
                        DocContent::Local => None,
                        DocContent::History(_) => None,
                        DocContent::Scratch { name, .. } => {
                            Some((PathBuf::from(name), is_pristine))
                        }
                    }
                } else {
                    None
                };
                let is_external = match (&path, &workspace_path) {
                    (Some((p, _)), Some(ws)) => is_external_file(p, ws),
                    _ => false,
                };
                let (icon, color, name, is_pristine) = match path {
                    Some((ref path, is_pristine)) => {
                        let (svg, color) = config.file_svg(path);
                        (
                            svg,
                            color,
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                            is_pristine,
                        )
                    }
                    None => (
                        config.ui_svg(LapceIcons::FILE),
                        Some(config.color(LapceColor::LAPCE_ICON_ACTIVE)),
                        "local".to_string(),
                        true,
                    ),
                };
                EditorTabChildViewInfo {
                    icon,
                    color,
                    name,
                    path: path.map(|opt| opt.0),
                    is_pristine,
                    is_external,
                }
            }),
            EditorTabChild::Settings(_) => create_memo(move |_| {
                let config = config.get();
                EditorTabChildViewInfo {
                    icon: config.ui_svg(LapceIcons::SETTINGS),
                    color: Some(config.color(LapceColor::LAPCE_ICON_ACTIVE)),
                    name: "Settings".to_string(),
                    path: None,
                    is_pristine: true,
                    is_external: false,
                }
            }),
            EditorTabChild::Keymap(_) => create_memo(move |_| {
                let config = config.get();
                EditorTabChildViewInfo {
                    icon: config.ui_svg(LapceIcons::KEYBOARD),
                    color: Some(config.color(LapceColor::LAPCE_ICON_ACTIVE)),
                    name: "Keyboard Shortcuts".to_string(),
                    path: None,
                    is_pristine: true,
                    is_external: false,
                }
            }),
            EditorTabChild::Projects(_) => create_memo(move |_| {
                let config = config.get();
                EditorTabChildViewInfo {
                    icon: config.ui_svg(LapceIcons::FILE_EXPLORER),
                    color: Some(config.color(LapceColor::LAPCE_ICON_ACTIVE)),
                    name: "Projects".to_string(),
                    path: None,
                    is_pristine: true,
                    is_external: false,
                }
            }),
        }
    }
}

/// A leaf node in the split tree representing a tabbed editor pane.
/// Contains a list of children (file editors, settings views, etc.) with exactly
/// one active child displayed at a time. Each child's tuple contains:
/// - RwSignal<usize>: the child's index (used by view for animation/ordering)
/// - RwSignal<Rect>: the child's tab header rect (used for drag-and-drop hit testing)
/// - EditorTabChild: the content reference
///
/// `locations` / `current_location` provide per-pane back/forward navigation,
/// independent of the global navigation history in MainSplitData.
#[derive(Clone)]
pub struct EditorTabData {
    pub scope: Scope,
    /// The parent split node this tab pane belongs to.
    pub split: SplitId,
    pub editor_tab_id: EditorTabId,
    /// Index into `children` of the currently visible tab.
    pub active: usize,
    pub children: Vec<(RwSignal<usize>, RwSignal<Rect>, EditorTabChild)>,
    pub window_origin: Point,
    pub layout_rect: Rect,
    /// Per-pane navigation history for local back/forward jumping.
    pub locations: RwSignal<im::Vector<EditorLocation>>,
    pub current_location: RwSignal<usize>,
}

impl EditorTabData {
    pub fn get_editor(
        &self,
        editors: Editors,
        path: &Path,
    ) -> Option<(usize, EditorData)> {
        for (i, child) in self.children.iter().enumerate() {
            if let (_, _, EditorTabChild::Editor(editor_id)) = child {
                if let Some(editor) = editors.editor_untracked(*editor_id) {
                    let is_path = editor.doc().content.with_untracked(|content| {
                        if let DocContent::File { path: p, .. } = content {
                            p == path
                        } else {
                            false
                        }
                    });
                    if is_path {
                        return Some((i, editor));
                    }
                }
            }
        }
        None
    }

    /// Finds the index of a child that matches the given source type.
    /// Used for tab deduplication: avoids opening a second tab for the same content.
    pub fn find_matching_child(
        &self,
        source: &EditorTabChildSource,
        editors: Editors,
    ) -> Option<usize> {
        match source {
            EditorTabChildSource::Editor { path, .. } => {
                self.get_editor(editors, path).map(|(i, _)| i)
            }
            EditorTabChildSource::NewFileEditor => None,
            EditorTabChildSource::Settings => {
                self.children.iter().position(|(_, _, child)| {
                    matches!(child, EditorTabChild::Settings(_))
                })
            }
            EditorTabChildSource::Keymap => {
                self.children.iter().position(|(_, _, child)| {
                    matches!(child, EditorTabChild::Keymap(_))
                })
            }
            EditorTabChildSource::Projects => {
                self.children.iter().position(|(_, _, child)| {
                    matches!(child, EditorTabChild::Projects(_))
                })
            }
        }
    }

    /// Finds the first reusable child slot when tabs are hidden (show_tab=false).
    /// A slot is reusable if it matches the source path or contains a pristine editor.
    /// Non-editor children (Settings, Keymap, etc.) are always reusable.
    pub fn find_reusable_child(
        &self,
        source: &EditorTabChildSource,
        editors: Editors,
    ) -> Option<usize> {
        for (i, (_, _, child)) in self.children.iter().enumerate() {
            let can_be_selected = match child {
                EditorTabChild::Editor(editor_id) => {
                    if let Some(editor) = editors.editor_untracked(*editor_id) {
                        let doc = editor.doc();
                        let same_path = if let EditorTabChildSource::Editor {
                            path,
                            ..
                        } = source
                        {
                            doc.content.with_untracked(|content| {
                                content.path().map(|p| p == path).unwrap_or(false)
                            })
                        } else {
                            false
                        };
                        same_path || doc.buffer.with_untracked(|b| b.is_pristine())
                    } else {
                        false
                    }
                }
                _ => true,
            };
            if can_be_selected {
                return Some(i);
            }
        }
        None
    }

    pub fn active_file_path(&self, editors: Editors) -> Option<PathBuf> {
        let (_, _, child) = self.children.get(self.active)?;
        if let EditorTabChild::Editor(editor_id) = child {
            editors.editor_untracked(*editor_id).and_then(|editor| {
                editor.doc().content.with_untracked(|content| {
                    if let DocContent::File { path, .. } = content {
                        Some(path.clone())
                    } else {
                        None
                    }
                })
            })
        } else {
            None
        }
    }

    pub fn tab_info(&self, data: &WorkspaceData) -> EditorTabInfo {
        EditorTabInfo {
            active: self.active,
            is_focus: data.main_split.active_editor_tab.get_untracked()
                == Some(self.editor_tab_id),
            children: self
                .children
                .iter()
                .map(|(_, _, child)| child.child_info(data))
                .collect(),
        }
    }
}
