#![allow(clippy::manual_clamp)]

pub mod directory;
pub mod encoding;
pub mod language;
pub mod lens;
pub mod meta;
pub mod rope_text_pos;
pub mod style;
pub mod syntax;

// Re-export everything from floem_editor_core (rope, buffer, commands, cursor,
// etc.) so that downstream crates can import them as `lapce_core::*` instead
// of depending on floem_editor_core directly. This keeps the dependency graph
// cleaner -- only lapce-core needs to know about the floem crate.
pub use floem_editor_core::*;
