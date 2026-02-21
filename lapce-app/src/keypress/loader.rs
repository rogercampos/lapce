use anyhow::{Result, anyhow};
use indexmap::IndexMap;

use super::keymap::{KeyMap, KeyMapPress};

/// Incrementally loads keybindings from multiple TOML sources, handling both
/// binding (positive) and unbinding (negative with "-" prefix) entries.
/// The two IndexMaps serve different lookup needs:
///   - `keymaps`: key sequence -> list of matching KeyMaps (for runtime matching)
///   - `command_keymaps`: command name -> list of KeyMaps (for the settings UI)
pub struct KeyMapLoader {
    keymaps: IndexMap<Vec<KeyMapPress>, Vec<KeyMap>>,
    command_keymaps: IndexMap<String, Vec<KeyMap>>,
}

impl KeyMapLoader {
    pub fn new() -> Self {
        Self {
            keymaps: Default::default(),
            command_keymaps: Default::default(),
        }
    }

    pub fn load_from_str<'a>(&'a mut self, s: &str) -> Result<&'a mut Self> {
        let toml_keymaps: toml_edit::Document = s.parse()?;
        let toml_keymaps = toml_keymaps
            .get("keymaps")
            .and_then(|v| v.as_array_of_tables())
            .ok_or_else(|| anyhow!("no keymaps"))?;

        let mut parse_failures = 0usize;
        for toml_keymap in toml_keymaps {
            let keymap = match Self::get_keymap(toml_keymap) {
                Ok(Some(keymap)) => keymap,
                Ok(None) => {
                    // Keymap ignored
                    continue;
                }
                Err(err) => {
                    parse_failures += 1;
                    tracing::warn!("Could not parse keymap: {err}");
                    continue;
                }
            };

            // Commands prefixed with "-" are unbind directives: they remove a
            // previously loaded keybinding with the same key+when combination.
            let (command, bind) = match keymap.command.strip_prefix('-') {
                Some(cmd) => (cmd.to_string(), false),
                None => (keymap.command.clone(), true),
            };

            let current_keymaps = self.command_keymaps.entry(command).or_default();
            if bind {
                current_keymaps.push(keymap.clone());
                // Register every prefix of the key sequence so the runtime can
                // detect partial matches (the Prefix state in KeymapMatch).
                for i in 1..keymap.key.len() + 1 {
                    let key = keymap.key[..i].to_vec();
                    self.keymaps.entry(key).or_default().push(keymap.clone());
                }
            } else {
                // Unbind: remove ALL matching keymaps from both lookup tables.
                let is_keymap = |k: &KeyMap| -> bool {
                    k.when == keymap.when && k.key == keymap.key
                };
                current_keymaps.retain(|k| !is_keymap(k));
                for i in 1..keymap.key.len() + 1 {
                    if let Some(keymaps) = self.keymaps.get_mut(&keymap.key[..i]) {
                        keymaps.retain(|k| !is_keymap(k));
                    }
                }
            }
        }

        if parse_failures > 0 {
            tracing::warn!(
                "{parse_failures} keymap(s) failed to parse, check logs for details"
            );
        }

        Ok(self)
    }

    #[allow(clippy::type_complexity)]
    pub fn finalize(
        self,
    ) -> (
        IndexMap<Vec<KeyMapPress>, Vec<KeyMap>>,
        IndexMap<String, Vec<KeyMap>>,
    ) {
        let Self {
            keymaps: map,
            command_keymaps: command_map,
        } = self;

        (map, command_map)
    }

    fn get_keymap(toml_keymap: &toml_edit::Table) -> Result<Option<KeyMap>> {
        let key = toml_keymap
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no key in keymap"))?;

        Ok(Some(KeyMap {
            key: KeyMapPress::parse(key),
            when: toml_keymap
                .get("when")
                .and_then(|w| w.as_str())
                .map(|w| w.to_string()),
            command: toml_keymap
                .get("command")
                .and_then(|c| c.as_str())
                .map(|w| w.trim().to_string())
                .unwrap_or_default(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use floem::keyboard::Key;

    use super::*;
    use crate::keypress::keymap::KeyMapKey;

    #[test]
    fn test_keymap() {
        let keymaps = r#"
[[keymaps]]
key = "ctrl+w l l"
command = "right"
when = "n"

[[keymaps]]
key = "ctrl+w l"
command = "right"
when = "n"

[[keymaps]]
key = "ctrl+w h"
command = "left"
when = "n"

[[keymaps]]
key = "ctrl+w"
command = "left"
when = "n"

[[keymaps]]
key = "End"
command = "line_end"
when = "n"

[[keymaps]]
key = "shift+i"
command = "insert_first_non_blank"
when = "n"
        
[[keymaps]]
key = "MouseForward"
command = "jump_location_forward"

[[keymaps]]
key = "MouseBackward"
command = "jump_location_backward"
        
[[keymaps]]
key = "Ctrl+MouseMiddle"
command = "goto_definition"
        "#;
        let mut loader = KeyMapLoader::new();
        loader.load_from_str(keymaps).unwrap();

        let (keymaps, _) = loader.finalize();

        // Lower case modifiers
        let keypress = KeyMapPress::parse("ctrl+w");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 4);

        let keypress = KeyMapPress::parse("ctrl+w l");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 2);

        let keypress = KeyMapPress::parse("ctrl+w h");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        let keypress = KeyMapPress::parse("ctrl+w l l");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        let keypress = KeyMapPress::parse("end");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        // Upper case modifiers
        let keypress = KeyMapPress::parse("Ctrl+w");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 4);

        let keypress = KeyMapPress::parse("Ctrl+w l");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 2);

        let keypress = KeyMapPress::parse("Ctrl+w h");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        let keypress = KeyMapPress::parse("Ctrl+w l l");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        let keypress = KeyMapPress::parse("End");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        // No modifier
        let keypress = KeyMapPress::parse("shift+i");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        // Mouse keys
        let keypress = KeyMapPress::parse("MouseForward");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        let keypress = KeyMapPress::parse("mousebackward");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        let keypress = KeyMapPress::parse("Ctrl+MouseMiddle");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);

        let keypress = KeyMapPress::parse("Ctrl++");
        assert_eq!(
            keypress[0].key,
            KeyMapKey::Logical(Key::Character("+".into()))
        );

        let keypress = KeyMapPress::parse("+");
        assert_eq!(
            keypress[0].key,
            KeyMapKey::Logical(Key::Character("+".into()))
        );
    }

    #[test]
    fn test_unbinding_removes_previous_binding() {
        let keymaps = r#"
[[keymaps]]
key = "Ctrl+s"
command = "save"

[[keymaps]]
key = "Ctrl+a"
command = "select_all"
        "#;
        let unbind = r#"
[[keymaps]]
key = "Ctrl+s"
command = "-save"
        "#;

        let mut loader = KeyMapLoader::new();
        loader.load_from_str(keymaps).unwrap();
        loader.load_from_str(unbind).unwrap();

        let (keymaps, command_keymaps) = loader.finalize();

        // The Ctrl+s binding should be removed from the key lookup
        let keypress = KeyMapPress::parse("Ctrl+s");
        let entries = keymaps.get(&keypress);
        assert!(
            entries.is_none() || entries.unwrap().is_empty(),
            "Ctrl+s should be unbound"
        );

        // The save command should have no bindings left
        let save_bindings = command_keymaps.get("save");
        assert!(
            save_bindings.is_none() || save_bindings.unwrap().is_empty(),
            "save command should have no bindings"
        );

        // The Ctrl+a binding should remain unaffected
        let keypress = KeyMapPress::parse("Ctrl+a");
        assert_eq!(keymaps.get(&keypress).unwrap().len(), 1);
    }

    #[test]
    fn test_unbinding_with_when_condition() {
        let keymaps = r#"
[[keymaps]]
key = "j"
command = "down"
when = "n"

[[keymaps]]
key = "j"
command = "other_down"
when = "v"
        "#;
        let unbind = r#"
[[keymaps]]
key = "j"
command = "-down"
when = "n"
        "#;

        let mut loader = KeyMapLoader::new();
        loader.load_from_str(keymaps).unwrap();
        loader.load_from_str(unbind).unwrap();

        let (keymaps, _) = loader.finalize();

        // Only one binding for "j" should remain (the "v" one)
        let keypress = KeyMapPress::parse("j");
        let entries = keymaps.get(&keypress).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "other_down");
        assert_eq!(entries[0].when.as_deref(), Some("v"));
    }

    #[test]
    fn test_missing_keymaps_section_errors() {
        let toml = r#"
[settings]
theme = "dark"
        "#;
        let mut loader = KeyMapLoader::new();
        let result = loader.load_from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_keymaps_section_errors() {
        // An empty document with no [[keymaps]] array of tables
        let toml = r#"
keymaps = []
        "#;
        let mut loader = KeyMapLoader::new();
        // toml_edit parses `keymaps = []` as a regular array, not array of
        // tables, so `as_array_of_tables()` returns None → error.
        let result = loader.load_from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_multiple_sources_accumulate() {
        let source1 = r#"
[[keymaps]]
key = "Ctrl+s"
command = "save"
        "#;
        let source2 = r#"
[[keymaps]]
key = "Ctrl+o"
command = "open"
        "#;

        let mut loader = KeyMapLoader::new();
        loader.load_from_str(source1).unwrap();
        loader.load_from_str(source2).unwrap();

        let (keymaps, command_keymaps) = loader.finalize();

        let ks = KeyMapPress::parse("Ctrl+s");
        assert_eq!(keymaps.get(&ks).unwrap().len(), 1);

        let ko = KeyMapPress::parse("Ctrl+o");
        assert_eq!(keymaps.get(&ko).unwrap().len(), 1);

        assert!(command_keymaps.contains_key("save"));
        assert!(command_keymaps.contains_key("open"));
    }

    #[test]
    fn test_chord_prefix_registration() {
        let keymaps = r#"
[[keymaps]]
key = "Ctrl+k Ctrl+s"
command = "save_all"
        "#;

        let mut loader = KeyMapLoader::new();
        loader.load_from_str(keymaps).unwrap();

        let (keymaps, _) = loader.finalize();

        // The prefix "Ctrl+k" should be registered
        let prefix = KeyMapPress::parse("Ctrl+k");
        assert!(keymaps.contains_key(&prefix));

        // The full chord should also be registered
        let full = KeyMapPress::parse("Ctrl+k Ctrl+s");
        assert!(keymaps.contains_key(&full));
    }

    #[test]
    fn test_unbinding_removes_all_duplicates() {
        // Load the same binding twice from two sources
        let source1 = r#"
[[keymaps]]
key = "Ctrl+s"
command = "save"
        "#;
        let source2 = r#"
[[keymaps]]
key = "Ctrl+s"
command = "save"
        "#;
        let unbind = r#"
[[keymaps]]
key = "Ctrl+s"
command = "-save"
        "#;

        let keypress = KeyMapPress::parse("Ctrl+s");

        // Verify we have 2 bindings before unbinding
        {
            let mut loader = KeyMapLoader::new();
            loader.load_from_str(source1).unwrap();
            loader.load_from_str(source2).unwrap();
            let (keymaps, _) = loader.finalize();
            assert_eq!(
                keymaps.get(&keypress).unwrap().len(),
                2,
                "Should have 2 bindings before unbind"
            );
        }

        let mut loader = KeyMapLoader::new();
        loader.load_from_str(source1).unwrap();
        loader.load_from_str(source2).unwrap();
        loader.load_from_str(unbind).unwrap();

        let (keymaps, command_keymaps) = loader.finalize();

        // Both duplicates should be removed
        let entries = keymaps.get(&keypress);
        assert!(
            entries.is_none() || entries.unwrap().is_empty(),
            "Both duplicate Ctrl+s bindings should be removed"
        );
        let save_bindings = command_keymaps.get("save");
        assert!(
            save_bindings.is_none() || save_bindings.unwrap().is_empty(),
            "All save command bindings should be removed"
        );
    }

    #[test]
    fn test_unbinding_chord_removes_from_all_prefixes() {
        let keymaps = r#"
[[keymaps]]
key = "Ctrl+k Ctrl+s"
command = "save_all"
        "#;
        let unbind = r#"
[[keymaps]]
key = "Ctrl+k Ctrl+s"
command = "-save_all"
        "#;

        let mut loader = KeyMapLoader::new();
        loader.load_from_str(keymaps).unwrap();
        loader.load_from_str(unbind).unwrap();

        let (keymaps, _) = loader.finalize();

        // Both the prefix and full chord entries should be empty
        let prefix = KeyMapPress::parse("Ctrl+k");
        let prefix_entries = keymaps.get(&prefix);
        assert!(prefix_entries.is_none() || prefix_entries.unwrap().is_empty());

        let full = KeyMapPress::parse("Ctrl+k Ctrl+s");
        let full_entries = keymaps.get(&full);
        assert!(full_entries.is_none() || full_entries.unwrap().is_empty());
    }
}
