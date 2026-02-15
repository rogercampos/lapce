use std::path::PathBuf;

use lapce_core::line_ending::LineEnding;

use crate::{editor::location::EditorLocation, workspace::LapceWorkspace};

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    pub content: PaletteItemContent,
    pub filter_text: String,
    pub score: u32,
    pub indices: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteItemContent {
    File {
        path: PathBuf,
        full_path: PathBuf,
    },
    Line {
        line: usize,
        content: String,
    },
    Workspace {
        workspace: LapceWorkspace,
    },
    Reference {
        path: PathBuf,
        location: EditorLocation,
    },
    Language {
        name: String,
    },
    LineEnding {
        kind: LineEnding,
    },
}
