use std::path::PathBuf;

use lsp_types::Position;
use serde::{Deserialize, Serialize};

/// Represents a single line from a file with its location. Used to display
/// reference results and search matches where we need to show the file path,
/// position, and the actual content of the line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLine {
    pub path: PathBuf,
    pub position: Position,
    pub content: String,
}
