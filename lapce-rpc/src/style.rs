use std::{collections::HashMap, path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize};

/// Maps line numbers to their styling information. The Arc allows sharing
/// across threads (proxy -> UI) without cloning the style vectors.
pub type LineStyles = HashMap<usize, Arc<Vec<LineStyle>>>;

/// A styled range within a single line. `start` and `end` are column offsets
/// (not byte offsets), making this directly usable for rendering.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LineStyle {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Style {
    pub fg_color: Option<String>,
}

/// Semantic token highlights from the LSP, covering an entire file.
/// Includes the document revision to allow the UI to discard stale results
/// if the document has been edited since the tokens were computed.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SemanticStyles {
    pub rev: u64,
    pub path: PathBuf,
    pub len: usize,
    pub styles: Vec<LineStyle>,
}
