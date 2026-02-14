use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ::core::slice;
use floem::{peniko::Color, prelude::palette::css};
use itertools::Itertools;
use lapce_core::directory::Directory;
use lapce_proxy::plugin::wasi::find_all_volts;
use lapce_rpc::plugin::VoltID;
use lsp_types::{CompletionItemKind, SymbolKind};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::Deserialize;
use strum::VariantNames;
use tracing::error;

use self::{
    color::LapceColor,
    color_theme::{ColorThemeConfig, ThemeColor, ThemeColorPreference},
    core::CoreConfig,
    editor::{EditorConfig, WrapStyle},
    icon::LapceIcons,
    icon_theme::IconThemeConfig,
    svg::SvgStore,
    ui::UIConfig,
};
use crate::workspace::LapceWorkspace;

pub mod color;
pub mod color_theme;
pub mod core;
pub mod editor;
pub mod icon;
pub mod icon_theme;
pub mod svg;
pub mod ui;
pub mod watcher;

// Embed default configuration files at compile time so the app can run without
// any on-disk defaults.
pub const LOGO: &str = include_str!("../../extra/images/logo.svg");
const DEFAULT_SETTINGS: &str = include_str!("../../defaults/settings.toml");
const DEFAULT_LIGHT_THEME: &str = include_str!("../../defaults/light-theme.toml");
const DEFAULT_DARK_THEME: &str = include_str!("../../defaults/dark-theme.toml");
const DEFAULT_ICON_THEME: &str = include_str!("../../defaults/icon-theme.toml");

// Lazily-initialized singletons: parsed once and cloned on each config reload.
// This avoids re-parsing embedded TOML on every config change.
static DEFAULT_CONFIG: Lazy<config::Config> = Lazy::new(LapceConfig::default_config);
static DEFAULT_LAPCE_CONFIG: Lazy<LapceConfig> =
    Lazy::new(LapceConfig::default_lapce_config);

static DEFAULT_DARK_THEME_CONFIG: Lazy<config::Config> = Lazy::new(|| {
    config::Config::builder()
        .add_source(config::File::from_str(
            DEFAULT_DARK_THEME,
            config::FileFormat::Toml,
        ))
        .build()
        .unwrap()
});

/// The default theme is the dark theme.
static DEFAULT_DARK_THEME_COLOR_CONFIG: Lazy<ColorThemeConfig> = Lazy::new(|| {
    let (_, theme) =
        LapceConfig::load_color_theme_from_str(DEFAULT_DARK_THEME).unwrap();
    theme.get::<ColorThemeConfig>("color-theme")
    .expect("Failed to load default dark theme. This is likely due to a missing or misnamed field in dark-theme.toml")
});

static DEFAULT_ICON_THEME_CONFIG: Lazy<config::Config> = Lazy::new(|| {
    config::Config::builder()
        .add_source(config::File::from_str(
            DEFAULT_ICON_THEME,
            config::FileFormat::Toml,
        ))
        .build()
        .unwrap()
});
static DEFAULT_ICON_THEME_ICON_CONFIG: Lazy<IconThemeConfig> = Lazy::new(|| {
    DEFAULT_ICON_THEME_CONFIG.get::<IconThemeConfig>("icon-theme")
    .expect("Failed to load default icon theme. This is likely due to a missing or misnamed field in icon-theme.toml")
});

/// Used for creating a `DropdownData` for a setting
#[derive(Debug, Clone)]
pub struct DropdownInfo {
    /// The currently selected item.
    pub active_index: usize,
    pub items: im::Vector<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct LapceConfig {
    // Monotonic ID derived from system time; changes on every config reload so
    // consumers can detect when the config object has been replaced.
    #[serde(skip)]
    pub id: u64,
    pub core: CoreConfig,
    pub ui: UIConfig,
    pub editor: EditorConfig,
    #[serde(default)]
    pub color_theme: ColorThemeConfig,
    #[serde(default)]
    pub icon_theme: IconThemeConfig,
    // Plugin config sections use `flatten` so that any top-level TOML key not
    // matching core/ui/editor/color_theme/icon_theme is captured as plugin config.
    #[serde(flatten)]
    pub plugins: HashMap<String, HashMap<String, serde_json::Value>>,
    #[serde(skip)]
    pub color: ThemeColor,
    #[serde(skip)]
    pub available_color_themes: HashMap<String, (String, config::Config)>,
    #[serde(skip)]
    pub available_icon_themes:
        HashMap<String, (String, config::Config, Option<PathBuf>)>,
    // #[serde(skip)]
    // tab_layout_info: Arc<RwLock<HashMap<(FontFamily, usize), f64>>>,
    #[serde(skip)]
    svg_store: Arc<RwLock<SvgStore>>,
    /// A list of the themes that are available. This is primarily for populating
    /// the theme picker, and serves as a cache.
    #[serde(skip)]
    color_theme_list: im::Vector<String>,
    #[serde(skip)]
    icon_theme_list: im::Vector<String>,
    /// The couple names for the wrap style
    #[serde(skip)]
    wrap_style_list: im::Vector<String>,
}

impl LapceConfig {
    pub fn load(
        workspace: &LapceWorkspace,
        disabled_volts: &[VoltID],
        extra_plugin_paths: &[PathBuf],
    ) -> Self {
        let config = Self::merge_config(workspace, None, None);
        let mut lapce_config: LapceConfig = match config.try_deserialize() {
            Ok(config) => config,
            Err(error) => {
                error!("Failed to deserialize configuration file: {error}");
                DEFAULT_LAPCE_CONFIG.clone()
            }
        };

        lapce_config.available_color_themes =
            Self::load_color_themes(disabled_volts, extra_plugin_paths);
        lapce_config.available_icon_themes =
            Self::load_icon_themes(disabled_volts, extra_plugin_paths);
        lapce_config.resolve_theme(workspace);

        lapce_config.color_theme_list = lapce_config
            .available_color_themes
            .values()
            .map(|(name, _)| name.clone())
            .sorted()
            .collect();

        lapce_config.icon_theme_list = lapce_config
            .available_icon_themes
            .values()
            .map(|(name, _, _)| name.clone())
            .sorted()
            .collect();

        lapce_config.wrap_style_list = im::vector![
            WrapStyle::None.to_string(),
            WrapStyle::EditorWidth.to_string(),
            // TODO: WrapStyle::WrapColumn.to_string(),
            WrapStyle::WrapWidth.to_string()
        ];

        lapce_config
    }

    /// Builds a merged config using the layered override strategy:
    ///   1. Embedded defaults (settings.toml)
    ///   2. Dark theme base colors (provides the default palette for themes)
    ///   3. Active color theme (overrides colors from step 2)
    ///   4. Active icon theme
    ///   5. User's global settings.toml (~/.config/.../settings.toml)
    ///   6. Workspace-local settings (.lapce/settings.toml in the project root)
    ///
    /// Each layer is added via `config::Config::builder().add_source()`, which
    /// means later sources override earlier ones for the same key. This gives
    /// workspace settings the highest priority.
    fn merge_config(
        workspace: &LapceWorkspace,
        color_theme_config: Option<config::Config>,
        icon_theme_config: Option<config::Config>,
    ) -> config::Config {
        let mut config = DEFAULT_CONFIG.clone();

        if let Some(theme) = color_theme_config {
            // Layer the dark theme first as a base, then the selected theme on top.
            // This ensures any color the theme doesn't define falls back to the
            // dark theme's value rather than being missing.
            config = config::Config::builder()
                .add_source(config.clone())
                .add_source(DEFAULT_DARK_THEME_CONFIG.clone())
                .add_source(theme)
                .build()
                .unwrap_or_else(|_| config.clone());
        }

        if let Some(theme) = icon_theme_config {
            config = config::Config::builder()
                .add_source(config.clone())
                .add_source(theme)
                .build()
                .unwrap_or_else(|_| config.clone());
        }

        // User-level global settings override theme values.
        if let Some(path) = Self::settings_file() {
            config = config::Config::builder()
                .add_source(config.clone())
                .add_source(config::File::from(path.as_path()).required(false))
                .build()
                .unwrap_or_else(|_| config.clone());
        }

        // Workspace-local settings get the highest priority.
        if let Some(path) = workspace.path.as_ref() {
            let path = path.join("./.lapce/settings.toml");
            config = config::Config::builder()
                .add_source(config.clone())
                .add_source(config::File::from(path.as_path()).required(false))
                .build()
                .unwrap_or_else(|_| config.clone());
        }

        config
    }

    fn update_id(&mut self) {
        self.id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
    }

    fn default_config() -> config::Config {
        config::Config::builder()
            .add_source(config::File::from_str(
                DEFAULT_SETTINGS,
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
    }

    fn default_lapce_config() -> LapceConfig {
        let mut default_lapce_config: LapceConfig =
            DEFAULT_CONFIG.clone().try_deserialize().expect("Failed to deserialize default config, this likely indicates a missing or misnamed field in settings.toml");
        default_lapce_config.color_theme = DEFAULT_DARK_THEME_COLOR_CONFIG.clone();
        default_lapce_config.icon_theme = DEFAULT_ICON_THEME_ICON_CONFIG.clone();
        default_lapce_config.resolve_colors(None);
        default_lapce_config
    }

    /// Re-merges the full config with the currently selected color and icon themes,
    /// then resolves all color variables to concrete Color values. Called whenever
    /// the theme selection changes or when config is first loaded.
    fn resolve_theme(&mut self, workspace: &LapceWorkspace) {
        let default_lapce_config = DEFAULT_LAPCE_CONFIG.clone();

        let color_theme_config = self
            .available_color_themes
            .get(&self.core.color_theme.to_lowercase())
            .map(|(_, config)| config)
            .unwrap_or(&DEFAULT_DARK_THEME_CONFIG);

        let icon_theme_config = self
            .available_icon_themes
            .get(&self.core.icon_theme.to_lowercase())
            .map(|(_, config, _)| config)
            .unwrap_or(&DEFAULT_ICON_THEME_CONFIG);

        let icon_theme_path = self
            .available_icon_themes
            .get(&self.core.icon_theme.to_lowercase())
            .map(|(_, _, path)| path);

        if let Ok(new) = Self::merge_config(
            workspace,
            Some(color_theme_config.clone()),
            Some(icon_theme_config.clone()),
        )
        .try_deserialize::<LapceConfig>()
        {
            self.core = new.core;
            self.ui = new.ui;
            self.editor = new.editor;
            self.color_theme = new.color_theme;
            self.icon_theme = new.icon_theme;
            if let Some(icon_theme_path) = icon_theme_path {
                self.icon_theme.path = icon_theme_path.clone().unwrap_or_default();
            }
            self.plugins = new.plugins;
        }
        self.resolve_colors(Some(&default_lapce_config));
        self.update_id();
    }

    /// Discovers all available color themes from three sources:
    ///   1. User-local theme files in the themes directory
    ///   2. Plugin-provided themes (from installed Volts)
    ///   3. Built-in light and dark themes (embedded at compile time)
    ///
    /// Built-in themes are inserted last, so they always override any local/plugin
    /// theme with the same name, guaranteeing the defaults are always available.
    fn load_color_themes(
        disabled_volts: &[VoltID],
        extra_plugin_paths: &[PathBuf],
    ) -> HashMap<String, (String, config::Config)> {
        let mut themes = Self::load_local_themes().unwrap_or_default();

        for (key, theme) in
            Self::load_plugin_color_themes(disabled_volts, extra_plugin_paths)
        {
            themes.insert(key, theme);
        }

        let (name, theme) =
            Self::load_color_theme_from_str(DEFAULT_LIGHT_THEME).unwrap();
        themes.insert(name.to_lowercase(), (name, theme));
        let (name, theme) =
            Self::load_color_theme_from_str(DEFAULT_DARK_THEME).unwrap();
        themes.insert(name.to_lowercase(), (name, theme));

        themes
    }

    pub fn default_color_theme(&self) -> &ColorThemeConfig {
        &DEFAULT_DARK_THEME_COLOR_CONFIG
    }

    /// Set the active color theme.
    /// Note that this does not save the config.
    pub fn set_color_theme(&mut self, workspace: &LapceWorkspace, theme: &str) {
        self.core.color_theme = theme.to_string();
        self.resolve_theme(workspace);
    }

    /// Set the active icon theme.  
    /// Note that this does not save the config.
    pub fn set_icon_theme(&mut self, workspace: &LapceWorkspace, theme: &str) {
        self.core.icon_theme = theme.to_string();
        self.resolve_theme(workspace);
    }

    /// Get the color by the name from the current theme if it exists
    /// Otherwise, get the color from the base them
    /// # Panics
    /// If the color was not able to be found in either theme, which may be indicative that
    /// it is misspelled or needs to be added to the base-theme.
    pub fn color(&self, name: &str) -> Color {
        match self.color.ui.get(name) {
            Some(c) => *c,
            None => {
                error!("Failed to find key: {name}");
                css::HOT_PINK
            }
        }
    }

    /// Retrieve a color value whose key starts with "style."
    pub fn style_color(&self, name: &str) -> Option<Color> {
        self.color.syntax.get(name).copied()
    }

    pub fn completion_color(
        &self,
        kind: Option<CompletionItemKind>,
    ) -> Option<Color> {
        let kind = kind?;
        let theme_str = match kind {
            CompletionItemKind::METHOD => "method",
            CompletionItemKind::FUNCTION => "method",
            CompletionItemKind::ENUM => "enum",
            CompletionItemKind::ENUM_MEMBER => "enum-member",
            CompletionItemKind::CLASS => "class",
            CompletionItemKind::VARIABLE => "field",
            CompletionItemKind::STRUCT => "structure",
            CompletionItemKind::KEYWORD => "keyword",
            CompletionItemKind::CONSTANT => "constant",
            CompletionItemKind::PROPERTY => "property",
            CompletionItemKind::FIELD => "field",
            CompletionItemKind::INTERFACE => "interface",
            CompletionItemKind::SNIPPET => "snippet",
            CompletionItemKind::MODULE => "builtinType",
            _ => "string",
        };

        self.style_color(theme_str)
    }

    /// Resolves the three color layers (base variables, UI colors, syntax colors)
    /// and determines the theme's light/dark/high-contrast preference based on
    /// the foreground vs background luminance comparison.
    fn resolve_colors(&mut self, default_config: Option<&LapceConfig>) {
        // First resolve base variables (e.g. "$red" -> "#E06C75") since UI and
        // syntax colors may reference them via "$variable" notation.
        self.color.base = self
            .color_theme
            .base
            .resolve(default_config.map(|c| &c.color_theme.base));
        self.color.ui = self
            .color_theme
            .resolve_ui_color(&self.color.base, default_config.map(|c| &c.color.ui));
        self.color.syntax = self.color_theme.resolve_syntax_color(
            &self.color.base,
            default_config.map(|c| &c.color.syntax),
        );

        // Heuristic: if the background RGB sum exceeds the foreground's,
        // the background is bright, i.e. it's a light theme.
        let fg = self.color(LapceColor::EDITOR_FOREGROUND).to_rgba8();
        let bg = self.color(LapceColor::EDITOR_BACKGROUND).to_rgba8();
        let is_light_theme = bg.r as u32 + bg.g as u32 + bg.b as u32
            > fg.r as u32 + fg.g as u32 + fg.b as u32;
        let high_contrast = self.color_theme.high_contrast.unwrap_or(false);
        self.color.color_preference = match (is_light_theme, high_contrast) {
            (true, true) => ThemeColorPreference::HighContrastLight,
            (false, true) => ThemeColorPreference::HighContrastDark,
            (true, false) => ThemeColorPreference::Light,
            (false, false) => ThemeColorPreference::Dark,
        };
    }

    fn load_local_themes() -> Option<HashMap<String, (String, config::Config)>> {
        let themes_folder = Directory::themes_directory()?;
        let themes: HashMap<String, (String, config::Config)> =
            std::fs::read_dir(themes_folder)
                .ok()?
                .filter_map(|entry| {
                    entry
                        .ok()
                        .and_then(|entry| Self::load_color_theme(&entry.path()))
                })
                .collect();
        Some(themes)
    }

    fn load_color_theme(path: &Path) -> Option<(String, (String, config::Config))> {
        if !path.is_file() {
            return None;
        }
        let config = config::Config::builder()
            .add_source(config::File::from(path))
            .build()
            .ok()?;
        let table = config.get_table("color-theme").ok()?;
        let name = table.get("name")?.to_string();
        Some((name.to_lowercase(), (name, config)))
    }

    /// Load the given theme by its contents.  
    /// Returns `(name, theme fields)`
    fn load_color_theme_from_str(s: &str) -> Option<(String, config::Config)> {
        let config = config::Config::builder()
            .add_source(config::File::from_str(s, config::FileFormat::Toml))
            .build()
            .ok()?;
        let table = config.get_table("color-theme").ok()?;
        let name = table.get("name")?.to_string();
        Some((name, config))
    }

    fn load_icon_themes(
        disabled_volts: &[VoltID],
        extra_plugin_paths: &[PathBuf],
    ) -> HashMap<String, (String, config::Config, Option<PathBuf>)> {
        let mut themes = HashMap::new();

        for (key, (name, theme, path)) in
            Self::load_plugin_icon_themes(disabled_volts, extra_plugin_paths)
        {
            themes.insert(key, (name, theme, Some(path)));
        }

        let (name, theme) =
            Self::load_icon_theme_from_str(DEFAULT_ICON_THEME).unwrap();
        themes.insert(name.to_lowercase(), (name, theme, None));

        themes
    }

    fn load_icon_theme_from_str(s: &str) -> Option<(String, config::Config)> {
        let config = config::Config::builder()
            .add_source(config::File::from_str(s, config::FileFormat::Toml))
            .build()
            .ok()?;
        let table = config.get_table("icon-theme").ok()?;
        let name = table.get("name")?.to_string();
        Some((name, config))
    }

    fn load_plugin_color_themes(
        disabled_volts: &[VoltID],
        extra_plugin_paths: &[PathBuf],
    ) -> HashMap<String, (String, config::Config)> {
        let mut themes: HashMap<String, (String, config::Config)> = HashMap::new();
        for meta in find_all_volts(extra_plugin_paths) {
            if disabled_volts.contains(&meta.id()) {
                continue;
            }
            if let Some(plugin_themes) = meta.color_themes.as_ref() {
                for theme_path in plugin_themes {
                    if let Some((key, theme)) =
                        Self::load_color_theme(&PathBuf::from(theme_path))
                    {
                        themes.insert(key, theme);
                    }
                }
            }
        }
        themes
    }

    fn load_plugin_icon_themes(
        disabled_volts: &[VoltID],
        extra_plugin_paths: &[PathBuf],
    ) -> HashMap<String, (String, config::Config, PathBuf)> {
        let mut themes: HashMap<String, (String, config::Config, PathBuf)> =
            HashMap::new();
        for meta in find_all_volts(extra_plugin_paths) {
            if disabled_volts.contains(&meta.id()) {
                continue;
            }
            if let Some(plugin_themes) = meta.icon_themes.as_ref() {
                for theme_path in plugin_themes {
                    if let Some((key, theme)) =
                        Self::load_icon_theme(&PathBuf::from(theme_path))
                    {
                        themes.insert(key, theme);
                    }
                }
            }
        }
        themes
    }

    fn load_icon_theme(
        path: &Path,
    ) -> Option<(String, (String, config::Config, PathBuf))> {
        if !path.is_file() {
            return None;
        }
        let config = config::Config::builder()
            .add_source(config::File::from(path))
            .build()
            .ok()?;
        let table = config.get_table("icon-theme").ok()?;
        let name = table.get("name")?.to_string();
        Some((
            name.to_lowercase(),
            (name, config, path.parent().unwrap().to_path_buf()),
        ))
    }

    pub fn export_theme(&self) -> String {
        let mut table = toml::value::Table::new();
        let mut theme = self.color_theme.clone();
        theme.name = "".to_string();
        table.insert(
            "color-theme".to_string(),
            toml::Value::try_from(&theme).unwrap(),
        );
        table.insert("ui".to_string(), toml::Value::try_from(&self.ui).unwrap());
        let value = toml::Value::Table(table);
        toml::to_string_pretty(&value).unwrap()
    }

    /// Returns the path to the user's global settings file, creating an empty
    /// file if it doesn't exist. Uses create_new to avoid race conditions.
    pub fn settings_file() -> Option<PathBuf> {
        let path = Directory::config_directory()?.join("settings.toml");

        if !path.exists() {
            if let Err(err) = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                tracing::error!("{:?}", err);
            }
        }

        Some(path)
    }

    pub fn keymaps_file() -> Option<PathBuf> {
        let path = Directory::config_directory()?.join("keymaps.toml");

        if !path.exists() {
            if let Err(err) = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                tracing::error!("{:?}", err);
            }
        }

        Some(path)
    }

    /// Resolves a UI icon name to its SVG string. Tries the active icon theme
    /// first (loading from disk), falling back to the embedded default codicon.
    pub fn ui_svg(&self, icon: &'static str) -> String {
        let svg = self.icon_theme.ui.get(icon).and_then(|path| {
            let path = self.icon_theme.path.join(path);
            self.svg_store.write().get_svg_on_disk(&path)
        });

        svg.unwrap_or_else(|| {
            let name = DEFAULT_ICON_THEME_ICON_CONFIG.ui.get(icon).unwrap();
            self.svg_store.write().get_default_svg(name)
        })
    }

    /// Resolves file paths to a file-type icon SVG and an optional tint color.
    /// Fallback chain:
    ///   1. Active plugin icon theme (on-disk SVGs) - color depends on use_editor_color
    ///   2. Default embedded filetype icons (no tint, retains original SVG colors)
    ///   3. Generic "file" icon with editor tint color
    pub fn files_svg(&self, paths: &[&Path]) -> (String, Option<Color>) {
        let svg = self
            .icon_theme
            .resolve_path_to_icon(paths)
            .and_then(|p| self.svg_store.write().get_svg_on_disk(&p));

        if let Some(svg) = svg {
            let color = if self.icon_theme.use_editor_color.unwrap_or(false) {
                Some(self.color(LapceColor::LAPCE_ICON_ACTIVE))
            } else {
                None
            };
            (svg, color)
        } else if let Some(icon_name) = DEFAULT_ICON_THEME_ICON_CONFIG
            .resolve_path_to_icon(paths)
            .and_then(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
        {
            let svg = self.svg_store.write().get_default_svg(&icon_name);
            (svg, None)
        } else {
            (
                self.ui_svg(LapceIcons::FILE),
                Some(self.color(LapceColor::LAPCE_ICON_ACTIVE)),
            )
        }
    }

    pub fn file_svg(&self, path: &Path) -> (String, Option<Color>) {
        self.files_svg(slice::from_ref(&path))
    }

    pub fn symbol_svg(&self, kind: &SymbolKind) -> Option<String> {
        let kind_str = match *kind {
            SymbolKind::ARRAY => LapceIcons::SYMBOL_KIND_ARRAY,
            SymbolKind::BOOLEAN => LapceIcons::SYMBOL_KIND_BOOLEAN,
            SymbolKind::CLASS => LapceIcons::SYMBOL_KIND_CLASS,
            SymbolKind::CONSTANT => LapceIcons::SYMBOL_KIND_CONSTANT,
            SymbolKind::ENUM_MEMBER => LapceIcons::SYMBOL_KIND_ENUM_MEMBER,
            SymbolKind::ENUM => LapceIcons::SYMBOL_KIND_ENUM,
            SymbolKind::EVENT => LapceIcons::SYMBOL_KIND_EVENT,
            SymbolKind::FIELD => LapceIcons::SYMBOL_KIND_FIELD,
            SymbolKind::FILE => LapceIcons::SYMBOL_KIND_FILE,
            SymbolKind::INTERFACE => LapceIcons::SYMBOL_KIND_INTERFACE,
            SymbolKind::KEY => LapceIcons::SYMBOL_KIND_KEY,
            SymbolKind::FUNCTION => LapceIcons::SYMBOL_KIND_FUNCTION,
            SymbolKind::METHOD => LapceIcons::SYMBOL_KIND_METHOD,
            SymbolKind::OBJECT => LapceIcons::SYMBOL_KIND_OBJECT,
            SymbolKind::NAMESPACE => LapceIcons::SYMBOL_KIND_NAMESPACE,
            SymbolKind::NUMBER => LapceIcons::SYMBOL_KIND_NUMBER,
            SymbolKind::OPERATOR => LapceIcons::SYMBOL_KIND_OPERATOR,
            SymbolKind::TYPE_PARAMETER => LapceIcons::SYMBOL_KIND_TYPE_PARAMETER,
            SymbolKind::PROPERTY => LapceIcons::SYMBOL_KIND_PROPERTY,
            SymbolKind::STRING => LapceIcons::SYMBOL_KIND_STRING,
            SymbolKind::STRUCT => LapceIcons::SYMBOL_KIND_STRUCT,
            SymbolKind::VARIABLE => LapceIcons::SYMBOL_KIND_VARIABLE,
            _ => return None,
        };

        Some(self.ui_svg(kind_str))
    }

    pub fn symbol_color(&self, kind: &SymbolKind) -> Option<Color> {
        let theme_str = match *kind {
            SymbolKind::METHOD => "method",
            SymbolKind::FUNCTION => "method",
            SymbolKind::ENUM => "enum",
            SymbolKind::ENUM_MEMBER => "enum-member",
            SymbolKind::CLASS => "class",
            SymbolKind::VARIABLE => "field",
            SymbolKind::STRUCT => "structure",
            SymbolKind::CONSTANT => "constant",
            SymbolKind::PROPERTY => "property",
            SymbolKind::FIELD => "field",
            SymbolKind::INTERFACE => "interface",
            SymbolKind::ARRAY => "",
            SymbolKind::BOOLEAN => "",
            SymbolKind::EVENT => "",
            SymbolKind::FILE => "",
            SymbolKind::KEY => "",
            SymbolKind::OBJECT => "",
            SymbolKind::NAMESPACE => "",
            SymbolKind::NUMBER => "number",
            SymbolKind::OPERATOR => "",
            SymbolKind::TYPE_PARAMETER => "",
            SymbolKind::STRING => "string",
            _ => return None,
        };

        self.style_color(theme_str)
    }

    pub fn logo_svg(&self) -> String {
        self.svg_store.read().logo_svg()
    }

    /// List of the color themes that are available by their display names.
    pub fn color_theme_list(&self) -> im::Vector<String> {
        self.color_theme_list.clone()
    }

    /// List of the icon themes that are available by their display names.
    pub fn icon_theme_list(&self) -> im::Vector<String> {
        self.icon_theme_list.clone()
    }

    /// Get the dropdown information for the specific setting, used for the settings UI.
    /// This should aim to efficiently return the data, because it is used to determine whether to
    /// update the dropdown items.
    pub fn get_dropdown_info(&self, kind: &str, key: &str) -> Option<DropdownInfo> {
        match (kind, key) {
            ("core", "color-theme") => Some(DropdownInfo {
                active_index: self
                    .color_theme_list
                    .iter()
                    .position(|s| s == &self.color_theme.name)
                    .unwrap_or(0),
                items: self.color_theme_list.clone(),
            }),
            ("core", "icon-theme") => Some(DropdownInfo {
                active_index: self
                    .icon_theme_list
                    .iter()
                    .position(|s| s == &self.icon_theme.name)
                    .unwrap_or(0),
                items: self.icon_theme_list.clone(),
            }),
            ("editor", "wrap-style") => Some(DropdownInfo {
                // TODO: it would be better to have the text not be the default kebab-case when
                // displayed in settings, but we would need to map back from the dropdown's value
                // or index.
                active_index: self
                    .wrap_style_list
                    .iter()
                    .flat_map(|w| WrapStyle::try_from_str(w))
                    .position(|w| w == self.editor.wrap_style)
                    .unwrap_or(0),
                items: self.wrap_style_list.clone(),
            }),
            ("ui", "tab-close-button") => {
                let items: Vec<String> = ui::TabCloseButton::VARIANTS
                    .iter()
                    .map(|s| s.to_string())
                    .sorted()
                    .collect();
                let active_str = format!("{:?}", self.ui.tab_close_button);
                let active_index =
                    items.iter().position(|s| s == &active_str).unwrap_or(0);
                Some(DropdownInfo {
                    active_index,
                    items: items.into(),
                })
            }
            ("ui", "tab-separator-height") => {
                let items: Vec<String> = ui::TabSeparatorHeight::VARIANTS
                    .iter()
                    .map(|s| s.to_string())
                    .sorted()
                    .collect();
                let active_str = format!("{:?}", self.ui.tab_separator_height);
                let active_index =
                    items.iter().position(|s| s == &active_str).unwrap_or(0);
                Some(DropdownInfo {
                    active_index,
                    items: items.into(),
                })
            }
            _ => None,
        }
    }

    /// Reads the user settings file as a TOML document, preserving formatting
    /// and comments. Returns None if the file can't be read or parsed.
    fn get_file_table() -> Option<toml_edit::Document> {
        let path = Self::settings_file()?;
        let content = std::fs::read_to_string(path).ok()?;
        let document: toml_edit::Document = content.parse().ok()?;
        Some(document)
    }

    pub fn reset_setting(parent: &str, key: &str) -> Option<()> {
        let mut main_table = Self::get_file_table().unwrap_or_default();

        // Find the container table
        let mut table = main_table.as_table_mut();
        for key in parent.split('.') {
            if !table.contains_key(key) {
                table.insert(
                    key,
                    toml_edit::Item::Table(toml_edit::Table::default()),
                );
            }
            table = table.get_mut(key)?.as_table_mut()?;
        }

        table.remove(key);

        // Store
        let path = Self::settings_file()?;
        std::fs::write(path, main_table.to_string().as_bytes()).ok()?;

        Some(())
    }

    /// Update the config file with the given edit.  
    /// This should be called whenever the configuration is changed, so that it is persisted.
    pub fn update_file(
        parent: &str,
        key: &str,
        value: toml_edit::Value,
    ) -> Option<()> {
        let mut main_table = Self::get_file_table().unwrap_or_default();

        // Find the container table
        let mut table = main_table.as_table_mut();
        for key in parent.split('.') {
            if !table.contains_key(key) {
                table.insert(
                    key,
                    toml_edit::Item::Table(toml_edit::Table::default()),
                );
            }
            table = table.get_mut(key)?.as_table_mut()?;
        }

        // Update key
        table.insert(key, toml_edit::Item::Value(value));

        // Store
        let path = Self::settings_file()?;
        std::fs::write(path, main_table.to_string().as_bytes()).ok()?;

        Some(())
    }
}
