pub struct LapceLayout {}

impl LapceLayout {
    /// Standard border radius used throughout the UI (modals, inputs, buttons, etc.)
    pub const BORDER_RADIUS: f64 = 6.0;

    /// Border radius for main panel containers (file explorer, search, bottom panel).
    pub const PANEL_BORDER_RADIUS: f64 = 10.0;

    /// Line height multiplier for UI lists, panels, and non-editor text.
    /// Also used as the default line height for parsed markdown.
    pub const UI_LINE_HEIGHT: f64 = 1.8;

    /// Maximum width/height percentage for floating modals (search, palette, etc.)
    pub const MODAL_MAX_PCT: f64 = 80.0;

    /// Default window width in pixels.
    pub const DEFAULT_WINDOW_WIDTH: f64 = 800.0;

    /// Default window height in pixels.
    pub const DEFAULT_WINDOW_HEIGHT: f64 = 600.0;

    /// Alpha multiplier for shadow/dimming overlays.
    pub const SHADOW_ALPHA: f32 = 0.5;
}
