use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    str::FromStr,
};

use floem::{peniko::Color, prelude::palette::css};
use serde::{Deserialize, Serialize};

use super::color::LoadThemeError;

#[derive(Debug, Clone, Default)]
pub enum ThemeColorPreference {
    #[default]
    Light,
    Dark,
    HighContrastDark,
    HighContrastLight,
}

/// Holds all the resolved theme variables
#[derive(Debug, Clone, Default)]
pub struct ThemeBaseColor(HashMap<String, Color>);
impl ThemeBaseColor {
    pub fn get(&self, name: &str) -> Option<Color> {
        self.0.get(name).map(ToOwned::to_owned)
    }
}

pub const THEME_RECURSION_LIMIT: usize = 6;

#[derive(Debug, Clone, Default)]
pub struct ThemeColor {
    pub color_preference: ThemeColorPreference,
    pub base: ThemeBaseColor,
    pub syntax: HashMap<String, Color>,
    pub ui: HashMap<String, Color>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ThemeBaseConfig(pub BTreeMap<String, String>);

impl ThemeBaseConfig {
    /// Resolve the variables in this theme base config into the actual colors.  
    /// The basic idea is just: `"field`: some value` does:
    /// - If the value does not start with `$`, then it is a color and we return it
    /// - If the value starts with `$` then it is a variable
    ///   - Look it up in the current theme
    ///   - If not found, look it up in the default theme
    ///   - If not found, return `Color::HOT_PINK` as a fallback
    ///
    /// Note that this applies even if the default theme colors have a variable.  
    /// This allows the default theme to have, for example, a `$uibg` variable that the current
    /// them can override so that if there's ever a new ui element using that variable, the theme
    /// does not have to be updated.
    pub fn resolve(&self, default: Option<&ThemeBaseConfig>) -> ThemeBaseColor {
        let default = default.cloned().unwrap_or_default();

        let mut base = ThemeBaseColor(HashMap::new());

        // We resolve all the variables to their values
        for (key, value) in self.0.iter() {
            match self.resolve_variable(&default, key, value, 0) {
                Ok(Some(color)) => {
                    let color = Color::from_str(color)
                        .unwrap_or_else(|_| {
                            tracing::warn!(
                                "Failed to parse color theme variable for ({key}: {value})"
                            );
                            css::HOT_PINK
                        });
                    base.0.insert(key.to_string(), color);
                }
                Ok(None) => {
                    tracing::warn!(
                        "Failed to resolve color theme variable for ({key}: {value})"
                    );
                }
                Err(err) => {
                    tracing::error!(
                        "Failed to resolve color theme variable ({key}: {value}): {err}"
                    );
                }
            }
        }

        base
    }

    /// Recursively resolves a single variable reference. A value starting with '$'
    /// is a reference to another variable. This method follows the chain until a
    /// literal color string is found, up to THEME_RECURSION_LIMIT depth.
    /// The lookup order is: current theme first, then defaults. This allows a
    /// custom theme to override base variables that the default theme defines.
    fn resolve_variable<'a>(
        &'a self,
        defaults: &'a ThemeBaseConfig,
        key: &str,
        value: &'a str,
        i: usize,
    ) -> Result<Option<&'a str>, LoadThemeError> {
        // Base case: not a variable reference, it's a literal color string.
        let Some(value) = value.strip_prefix('$') else {
            return Ok(Some(value));
        };

        if i > THEME_RECURSION_LIMIT {
            return Err(LoadThemeError::RecursionLimitReached {
                variable_name: key.to_string(),
            });
        }

        let target =
            self.get(value)
                .or_else(|| defaults.get(value))
                .ok_or_else(|| LoadThemeError::VariableNotFound {
                    variable_name: key.to_string(),
                })?;

        self.resolve_variable(defaults, value, target, i + 1)
    }

    // Note: this returns an `&String` just to make it consistent with hashmap lookups that are
    // also used via ui/syntax
    pub fn get(&self, name: &str) -> Option<&String> {
        self.0.get(name)
    }

    pub fn key_values(&self) -> BTreeMap<String, String> {
        self.0.clone()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct ColorThemeConfig {
    #[serde(skip)]
    pub path: PathBuf,
    pub name: String,
    pub high_contrast: Option<bool>,
    pub base: ThemeBaseConfig,
    pub syntax: BTreeMap<String, String>,
    pub ui: BTreeMap<String, String>,
}

impl ColorThemeConfig {
    /// Resolves a map of color definitions (ui or syntax) to concrete Color values.
    /// Each value is either:
    ///   - A "$variable" reference resolved through the base color palette
    ///   - A literal hex color string like "#FF0000"
    /// If neither resolves, falls back to the default theme's value for that key,
    /// and finally to black as a last resort.
    fn resolve_color(
        colors: &BTreeMap<String, String>,
        base: &ThemeBaseColor,
        default: Option<&HashMap<String, Color>>,
    ) -> HashMap<String, Color> {
        colors
            .iter()
            .map(|(name, hex)| {
                let color = if let Some(stripped) = hex.strip_prefix('$') {
                    base.get(stripped)
                } else {
                    Color::from_str(hex).ok()
                };

                let color = color
                    .or_else(|| {
                        default.and_then(|default| default.get(name).cloned())
                    })
                    .unwrap_or(Color::from_rgb8(0, 0, 0));

                (name.to_string(), color)
            })
            .collect()
    }

    pub(super) fn resolve_ui_color(
        &self,
        base: &ThemeBaseColor,
        default: Option<&HashMap<String, Color>>,
    ) -> HashMap<String, Color> {
        Self::resolve_color(&self.ui, base, default)
    }

    pub(super) fn resolve_syntax_color(
        &self,
        base: &ThemeBaseColor,
        default: Option<&HashMap<String, Color>>,
    ) -> HashMap<String, Color> {
        Self::resolve_color(&self.syntax, base, default)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use config::Config;
    use floem::{peniko::Color, prelude::palette::css};

    use crate::{config::LapceConfig, workspace::LapceWorkspace};

    use super::*;

    // --- resolve_variable() ---

    fn make_base(entries: &[(&str, &str)]) -> ThemeBaseConfig {
        ThemeBaseConfig(
            entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn resolve_variable_literal_value() {
        let theme = make_base(&[("red", "#FF0000")]);
        let defaults = ThemeBaseConfig::default();
        let result = theme.resolve_variable(&defaults, "red", "#FF0000", 0);
        assert_eq!(result.unwrap(), Some("#FF0000"));
    }

    #[test]
    fn resolve_variable_single_hop() {
        let theme = make_base(&[("bg", "$red"), ("red", "#FF0000")]);
        let defaults = ThemeBaseConfig::default();
        let result = theme.resolve_variable(&defaults, "bg", "$red", 0);
        assert_eq!(result.unwrap(), Some("#FF0000"));
    }

    #[test]
    fn resolve_variable_chain_resolution() {
        let theme = make_base(&[("a", "$b"), ("b", "$c"), ("c", "#123456")]);
        let defaults = ThemeBaseConfig::default();
        let result = theme.resolve_variable(&defaults, "a", "$b", 0);
        assert_eq!(result.unwrap(), Some("#123456"));
    }

    #[test]
    fn resolve_variable_falls_back_to_defaults() {
        let theme = make_base(&[("bg", "$primary")]);
        let defaults = make_base(&[("primary", "#AABBCC")]);
        let result = theme.resolve_variable(&defaults, "bg", "$primary", 0);
        assert_eq!(result.unwrap(), Some("#AABBCC"));
    }

    #[test]
    fn resolve_variable_recursion_limit() {
        // The check `i > THEME_RECURSION_LIMIT` fires after stripping '$',
        // so we need enough '$' hops to reach i=7 (>6).
        // a->b->c->d->e->f->g->h->z (8 variable hops)
        let theme = make_base(&[
            ("a", "$b"),
            ("b", "$c"),
            ("c", "$d"),
            ("d", "$e"),
            ("e", "$f"),
            ("f", "$g"),
            ("g", "$h"),
            ("h", "$z"),
            ("z", "#000000"),
        ]);
        let defaults = ThemeBaseConfig::default();
        let result = theme.resolve_variable(&defaults, "a", "$b", 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            crate::config::color::LoadThemeError::RecursionLimitReached { .. }
        ));
    }

    #[test]
    fn resolve_variable_not_found() {
        let theme = make_base(&[("bg", "$nonexistent")]);
        let defaults = ThemeBaseConfig::default();
        let result = theme.resolve_variable(&defaults, "bg", "$nonexistent", 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            crate::config::color::LoadThemeError::VariableNotFound { .. }
        ));
    }

    // --- resolve_color() ---

    #[test]
    fn resolve_color_hex_literal() {
        let mut colors = BTreeMap::new();
        colors.insert("my.color".to_string(), "#FF0000".to_string());
        let base = ThemeBaseColor::default();
        let resolved = ColorThemeConfig::resolve_color(&colors, &base, None);
        assert_eq!(
            resolved.get("my.color").unwrap(),
            &Color::from_rgb8(0xFF, 0, 0)
        );
    }

    #[test]
    fn resolve_color_variable_reference() {
        let mut base_map = HashMap::new();
        base_map.insert("red".to_string(), Color::from_rgb8(0xFF, 0, 0));
        let base = ThemeBaseColor(base_map);

        let mut colors = BTreeMap::new();
        colors.insert("my.color".to_string(), "$red".to_string());

        let resolved = ColorThemeConfig::resolve_color(&colors, &base, None);
        assert_eq!(
            resolved.get("my.color").unwrap(),
            &Color::from_rgb8(0xFF, 0, 0)
        );
    }

    #[test]
    fn resolve_color_falls_back_to_default_map() {
        let base = ThemeBaseColor::default();
        let mut colors = BTreeMap::new();
        // Reference a variable that doesn't exist in base
        colors.insert("my.color".to_string(), "$nonexistent".to_string());

        let mut default_map = HashMap::new();
        default_map.insert("my.color".to_string(), Color::from_rgb8(0, 0xFF, 0));

        let resolved =
            ColorThemeConfig::resolve_color(&colors, &base, Some(&default_map));
        assert_eq!(
            resolved.get("my.color").unwrap(),
            &Color::from_rgb8(0, 0xFF, 0)
        );
    }

    #[test]
    fn resolve_color_falls_back_to_black() {
        let base = ThemeBaseColor::default();
        let mut colors = BTreeMap::new();
        colors.insert("my.color".to_string(), "$nonexistent".to_string());

        let resolved = ColorThemeConfig::resolve_color(&colors, &base, None);
        assert_eq!(
            resolved.get("my.color").unwrap(),
            &Color::from_rgb8(0, 0, 0)
        );
    }

    #[test]
    fn resolve_color_invalid_hex_falls_back_to_black() {
        let base = ThemeBaseColor::default();
        let mut colors = BTreeMap::new();
        colors.insert("my.color".to_string(), "not-a-color".to_string());

        let resolved = ColorThemeConfig::resolve_color(&colors, &base, None);
        // Invalid hex literal with no default → black
        assert_eq!(
            resolved.get("my.color").unwrap(),
            &Color::from_rgb8(0, 0, 0)
        );
    }

    // --- resolve() integration ---

    #[test]
    fn resolve_produces_colors_from_literals() {
        let theme = make_base(&[("red", "#FF0000"), ("blue", "#0000FF")]);
        let base = theme.resolve(None);
        assert_eq!(base.get("red").unwrap(), Color::from_rgb8(0xFF, 0, 0));
        assert_eq!(base.get("blue").unwrap(), Color::from_rgb8(0, 0, 0xFF));
    }

    #[test]
    fn resolve_follows_variable_references() {
        let theme = make_base(&[("bg", "$red"), ("red", "#FF0000")]);
        let base = theme.resolve(None);
        assert_eq!(base.get("bg").unwrap(), Color::from_rgb8(0xFF, 0, 0));
    }

    // --- existing integration test ---

    #[test]
    fn test_resolve() {
        // Mimicking load
        let workspace = LapceWorkspace::default();

        let config = LapceConfig::merge_config(&workspace, None, None);
        let mut lapce_config: LapceConfig = config.try_deserialize().unwrap();

        let test_theme_str = r##"
[color-theme]
name = "test"
color-preference = "dark"

[ui]

[color-theme.base]
"blah" = "#ff00ff"
"text" = "#000000"

[color-theme.syntax]

[color-theme.ui]
"lapce.error" = "#ffffff"
"editor.background" = "$blah"
"##;
        println!("Test theme: {test_theme_str}");
        let test_theme_cfg = Config::builder()
            .add_source(config::File::from_str(
                test_theme_str,
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap();

        lapce_config.available_color_themes =
            [("test".to_string(), ("test".to_string(), test_theme_cfg))]
                .into_iter()
                .collect();
        // lapce_config.available_icon_themes = Some(vec![]);
        lapce_config.core.color_theme = "test".to_string();

        lapce_config.resolve_theme(&workspace);

        println!("Hot Pink: {:?}", css::HOT_PINK);
        // test basic override
        assert_eq!(
            lapce_config.color("lapce.error"),
            Color::WHITE,
            "Failed to get basic theme override"
        );
        // test that it falls through to the dark theme for unspecified color
        assert_eq!(
            lapce_config.color("lapce.warn"),
            Color::from_rgb8(0xE5, 0xC0, 0x7B),
            "Failed to get from fallback dark theme"
        ); // $yellow
        // test that our custom variable worked
        assert_eq!(
            lapce_config.color("editor.background"),
            Color::from_rgb8(0xFF, 0x00, 0xFF),
            "Failed to get from custom variable"
        );
        // test that for text it now uses our redeclared variable
        assert_eq!(
            lapce_config.color("editor.foreground"),
            Color::BLACK,
            "Failed to get from custom variable circle back around"
        );

        // don't bother filling color/icon theme list
        // don't bother with wrap style list
        // don't bother with terminal colors
    }
}
