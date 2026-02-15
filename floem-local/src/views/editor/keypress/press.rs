use std::fmt::Display;

use crate::keyboard::{Key, KeyCode, KeyEvent, Modifiers, NamedKey, PhysicalKey};

use super::key::KeyInput;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyPress {
    pub key: KeyInput,
    pub mods: Modifiers,
}

impl KeyPress {
    pub fn new(key: KeyInput, mods: Modifiers) -> Self {
        Self { key, mods }
    }

    pub fn to_lowercase(&self) -> Self {
        let key = match &self.key {
            KeyInput::Keyboard(Key::Character(c), key_code) => KeyInput::Keyboard(
                Key::Character(c.to_lowercase().into()),
                *key_code,
            ),
            _ => self.key.clone(),
        };
        Self {
            key,
            mods: self.mods,
        }
    }

    pub fn is_char(&self) -> bool {
        let mut mods = self.mods;
        mods.set(Modifiers::SHIFT, false);
        if mods.is_empty() {
            if let KeyInput::Keyboard(Key::Character(_c), _) = &self.key {
                return true;
            }
        }
        false
    }

    pub fn is_modifiers(&self) -> bool {
        if let KeyInput::Keyboard(_, scancode) = &self.key {
            matches!(
                scancode,
                PhysicalKey::Code(KeyCode::Meta)
                    | PhysicalKey::Code(KeyCode::SuperLeft)
                    | PhysicalKey::Code(KeyCode::SuperRight)
                    | PhysicalKey::Code(KeyCode::ShiftLeft)
                    | PhysicalKey::Code(KeyCode::ShiftRight)
                    | PhysicalKey::Code(KeyCode::ControlLeft)
                    | PhysicalKey::Code(KeyCode::ControlRight)
                    | PhysicalKey::Code(KeyCode::AltLeft)
                    | PhysicalKey::Code(KeyCode::AltRight)
            )
        } else {
            false
        }
    }

    pub fn label(&self) -> String {
        let mut keys = String::from("");
        if self.mods.control() {
            keys.push_str("Ctrl+");
        }
        if self.mods.alt() {
            keys.push_str("Alt+");
        }
        if self.mods.meta() {
            let keyname = match std::env::consts::OS {
                "macos" => "Cmd+",
                "windows" => "Win+",
                _ => "Meta+",
            };
            keys.push_str(keyname);
        }
        if self.mods.shift() {
            keys.push_str("Shift+");
        }
        keys.push_str(&self.key.to_string());
        keys.trim().to_string()
    }

    pub fn parse(key: &str) -> Vec<Self> {
        key.split(' ')
            .filter_map(|k| {
                let (modifiers, key) = match k.rsplit_once('+') {
                    Some(pair) => pair,
                    None => ("", k),
                };

                let key = match key.parse().ok() {
                    Some(key) => key,
                    None => {
                        // Skip past unrecognized key definitions
                        // warn!("Unrecognized key: {key}");
                        return None;
                    }
                };

                let mut mods = Modifiers::empty();
                for part in modifiers.to_lowercase().split('+') {
                    match part {
                        "ctrl" => mods.set(Modifiers::CONTROL, true),
                        "meta" => mods.set(Modifiers::META, true),
                        "shift" => mods.set(Modifiers::SHIFT, true),
                        "alt" => mods.set(Modifiers::ALT, true),
                        "altgr" => mods.set(Modifiers::ALT, true),
                        "" => (),
                        // other => warn!("Invalid key modifier: {}", other),
                        _ => {}
                    }
                }

                Some(KeyPress { key, mods })
            })
            .collect()
    }
}

impl Display for KeyPress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.mods.contains(Modifiers::CONTROL) {
            let _ = f.write_str("Ctrl+");
        }
        if self.mods.contains(Modifiers::ALT) {
            let _ = f.write_str("Alt+");
        }
        if self.mods.contains(Modifiers::META) {
            let _ = f.write_str("Meta+");
        }
        if self.mods.contains(Modifiers::SHIFT) {
            let _ = f.write_str("Shift+");
        }
        if self.mods.contains(Modifiers::ALTGR) {
            let _ = f.write_str("Altgr+");
        }
        f.write_str(&self.key.to_string())
    }
}
impl TryFrom<&KeyEvent> for KeyPress {
    type Error = ();

    fn try_from(ev: &KeyEvent) -> Result<Self, Self::Error> {
        Ok(KeyPress {
            key: KeyInput::Keyboard(ev.key.logical_key.clone(), ev.key.physical_key),
            mods: get_key_modifiers(ev),
        })
    }
}

pub fn get_key_modifiers(key_event: &KeyEvent) -> Modifiers {
    let mut mods = key_event.modifiers;

    match &key_event.key.logical_key {
        Key::Named(NamedKey::Shift) => mods.set(Modifiers::SHIFT, false),
        Key::Named(NamedKey::Alt) => mods.set(Modifiers::ALT, false),
        Key::Named(NamedKey::Meta) => mods.set(Modifiers::META, false),
        Key::Named(NamedKey::Control) => mods.set(Modifiers::CONTROL, false),
        _ => (),
    }

    mods
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- KeyPress::parse ---------------------------------------------------

    #[test]
    fn parse_simple_key() {
        let presses = KeyPress::parse("a");
        assert_eq!(presses.len(), 1);
        assert_eq!(
            presses[0].key,
            KeyInput::Keyboard(
                Key::Character("a".into()),
                PhysicalKey::Code(KeyCode::KeyA)
            )
        );
        assert!(presses[0].mods.is_empty());
    }

    #[test]
    fn parse_with_ctrl_modifier() {
        let presses = KeyPress::parse("Ctrl+s");
        assert_eq!(presses.len(), 1);
        assert_eq!(presses[0].mods, Modifiers::CONTROL);
        assert_eq!(
            presses[0].key,
            KeyInput::Keyboard(
                Key::Character("s".into()),
                PhysicalKey::Code(KeyCode::KeyS)
            )
        );
    }

    #[test]
    fn parse_multiple_modifiers() {
        let presses = KeyPress::parse("Ctrl+Shift+Alt+a");
        assert_eq!(presses.len(), 1);
        assert_eq!(
            presses[0].mods,
            Modifiers::CONTROL | Modifiers::SHIFT | Modifiers::ALT
        );
    }

    #[test]
    fn display_ctrl_alt_a() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Character("a".into()),
                PhysicalKey::Code(KeyCode::KeyA),
            ),
            Modifiers::CONTROL | Modifiers::ALT,
        );
        let s = kp.to_string();
        assert!(s.contains("Ctrl+"), "got: {s}");
        assert!(s.contains("Alt+"), "got: {s}");
        // KeyInput Display uses uppercase for letter keys (KeyA -> "A")
        assert!(s.ends_with('A'), "got: {s}");
    }

    #[test]
    fn parse_chord_sequence() {
        let presses = KeyPress::parse("Ctrl+k Ctrl+s");
        assert_eq!(presses.len(), 2);
        assert_eq!(presses[0].mods, Modifiers::CONTROL);
        assert_eq!(presses[1].mods, Modifiers::CONTROL);
    }

    #[test]
    fn parse_named_key() {
        let presses = KeyPress::parse("Escape");
        assert_eq!(presses.len(), 1);
        assert_eq!(
            presses[0].key,
            KeyInput::Keyboard(
                Key::Named(NamedKey::Escape),
                PhysicalKey::Code(KeyCode::Escape)
            )
        );
    }

    #[test]
    fn parse_unrecognized_key_skipped() {
        let presses = KeyPress::parse("Ctrl+NOTAKEY");
        assert!(presses.is_empty());
    }

    #[test]
    fn parse_altgr_maps_to_alt() {
        let presses = KeyPress::parse("Altgr+a");
        assert_eq!(presses.len(), 1);
        // altgr is treated as alt in the parser
        assert_eq!(presses[0].mods, Modifiers::ALT);
    }

    // -- KeyPress::is_char -------------------------------------------------

    #[test]
    fn is_char_plain_character() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Character("a".into()),
                PhysicalKey::Code(KeyCode::KeyA),
            ),
            Modifiers::empty(),
        );
        assert!(kp.is_char());
    }

    #[test]
    fn is_char_with_shift_is_true() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Character("A".into()),
                PhysicalKey::Code(KeyCode::KeyA),
            ),
            Modifiers::SHIFT,
        );
        assert!(kp.is_char());
    }

    #[test]
    fn is_char_with_ctrl_is_false() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Character("a".into()),
                PhysicalKey::Code(KeyCode::KeyA),
            ),
            Modifiers::CONTROL,
        );
        assert!(!kp.is_char());
    }

    #[test]
    fn is_char_named_key_is_false() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Named(NamedKey::Enter),
                PhysicalKey::Code(KeyCode::Enter),
            ),
            Modifiers::empty(),
        );
        assert!(!kp.is_char());
    }

    // -- KeyPress::is_modifiers --------------------------------------------

    #[test]
    fn is_modifiers_shift_left() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Named(NamedKey::Shift),
                PhysicalKey::Code(KeyCode::ShiftLeft),
            ),
            Modifiers::SHIFT,
        );
        assert!(kp.is_modifiers());
    }

    #[test]
    fn is_modifiers_control_right() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Named(NamedKey::Control),
                PhysicalKey::Code(KeyCode::ControlRight),
            ),
            Modifiers::CONTROL,
        );
        assert!(kp.is_modifiers());
    }

    #[test]
    fn is_modifiers_meta() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Named(NamedKey::Meta),
                PhysicalKey::Code(KeyCode::Meta),
            ),
            Modifiers::META,
        );
        assert!(kp.is_modifiers());
    }

    #[test]
    fn is_modifiers_regular_key_false() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Character("a".into()),
                PhysicalKey::Code(KeyCode::KeyA),
            ),
            Modifiers::empty(),
        );
        assert!(!kp.is_modifiers());
    }

    // -- KeyPress::to_lowercase --------------------------------------------

    #[test]
    fn to_lowercase_character() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Character("A".into()),
                PhysicalKey::Code(KeyCode::KeyA),
            ),
            Modifiers::SHIFT,
        );
        let lower = kp.to_lowercase();
        assert_eq!(
            lower.key,
            KeyInput::Keyboard(
                Key::Character("a".into()),
                PhysicalKey::Code(KeyCode::KeyA)
            )
        );
        assert_eq!(lower.mods, Modifiers::SHIFT);
    }

    #[test]
    fn to_lowercase_named_key_unchanged() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Named(NamedKey::Enter),
                PhysicalKey::Code(KeyCode::Enter),
            ),
            Modifiers::CONTROL,
        );
        let lower = kp.to_lowercase();
        assert_eq!(lower.key, kp.key);
    }

    // -- KeyPress::label ---------------------------------------------------

    #[test]
    fn label_ctrl_a() {
        let kp = KeyPress::parse("Ctrl+a");
        assert_eq!(kp.len(), 1);
        let label = kp[0].label();
        assert!(label.starts_with("Ctrl+"));
        // KeyInput Display uses uppercase for letter keys (KeyA -> "A")
        assert!(label.ends_with('A'), "got: {label}");
    }

    #[test]
    fn label_no_modifiers() {
        let kp = KeyPress::parse("Escape");
        assert_eq!(kp.len(), 1);
        let label = kp[0].label();
        assert_eq!(label, "Escape");
    }

    #[test]
    fn label_meta_key() {
        let kp = KeyPress::parse("Meta+q");
        assert_eq!(kp.len(), 1);
        let label = kp[0].label();
        // on macOS it should be Cmd+, on windows Win+, else Meta+
        let expected_prefix = match std::env::consts::OS {
            "macos" => "Cmd+",
            "windows" => "Win+",
            _ => "Meta+",
        };
        assert!(label.starts_with(expected_prefix), "got: {label}");
    }

    // -- Display -----------------------------------------------------------

    #[test]
    fn display_no_mods() {
        let kp = KeyPress::new(
            KeyInput::Keyboard(
                Key::Named(NamedKey::Space),
                PhysicalKey::Code(KeyCode::Space),
            ),
            Modifiers::empty(),
        );
        let s = kp.to_string();
        assert_eq!(s, "Space");
    }
}
