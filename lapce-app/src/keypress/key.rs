use floem::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

use super::keymap::KeyMapKey;

#[derive(Clone, Debug)]
pub(crate) enum KeyInput {
    Keyboard {
        physical: PhysicalKey,
        logical: Key,
        location: KeyLocation,
        key_without_modifiers: Key,
        repeat: bool,
    },
    Pointer(floem::pointer::PointerButton),
}

impl KeyInput {
    /// Converts a raw key event into the canonical form used for keymap lookup.
    /// This normalization ensures that keybindings work consistently:
    ///   - Repeated modifier keys are ignored (holding Ctrl shouldn't keep matching)
    ///   - Numpad keys use their logical identity (e.g. Numpad1 matches "1")
    ///   - ASCII characters are lowercased for case-insensitive matching
    ///   - Non-ASCII characters fall back to physical key codes (for intl layouts)
    ///   - Dead keys and unidentified keys use physical codes as a last resort
    pub fn keymap_key(&self) -> Option<KeyMapKey> {
        if let KeyInput::Keyboard {
            repeat, logical, ..
        } = self
        {
            // Suppress auto-repeat events for modifier keys. Holding a modifier
            // generates repeat events that shouldn't trigger additional keybindings.
            if *repeat
                && (matches!(
                    logical,
                    Key::Named(NamedKey::Meta)
                        | Key::Named(NamedKey::Shift)
                        | Key::Named(NamedKey::Alt)
                        | Key::Named(NamedKey::Control),
                ))
            {
                return None;
            }
        }

        Some(match self {
            KeyInput::Pointer(b) => KeyMapKey::Pointer(*b),
            KeyInput::Keyboard {
                physical,
                key_without_modifiers,
                logical,
                location,
                ..
            } => {
                // Numpad keys are matched by their logical meaning so "Enter"
                // matches both the main and numpad Enter keys.
                #[allow(clippy::single_match)]
                match location {
                    KeyLocation::Numpad => {
                        return Some(KeyMapKey::Logical(logical.to_owned()));
                    }
                    _ => {}
                }

                match key_without_modifiers {
                    Key::Named(_) => {
                        KeyMapKey::Logical(key_without_modifiers.to_owned())
                    }
                    Key::Character(c) => {
                        if c == " " {
                            KeyMapKey::Logical(Key::Named(NamedKey::Space))
                        } else if c.len() == 1 && c.is_ascii() {
                            // Lowercase ASCII so "a" and "A" match the same binding.
                            KeyMapKey::Logical(Key::Character(
                                c.to_lowercase().into(),
                            ))
                        } else {
                            // Non-ASCII chars (e.g. on German/French layouts) use
                            // physical key position for consistent matching.
                            KeyMapKey::Physical(*physical)
                        }
                    }
                    Key::Unidentified(_) => KeyMapKey::Physical(*physical),
                    Key::Dead(_) => KeyMapKey::Physical(*physical),
                }
            }
        })
    }
}
