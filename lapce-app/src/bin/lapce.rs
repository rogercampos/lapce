// On Windows release builds, use the "windows" subsystem to avoid spawning a console window.
// Debug builds keep the console for easier development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use lapce_app::app;

/// Thin entry point that delegates to app::launch() where all initialization happens.
pub fn main() {
    app::launch();
}
