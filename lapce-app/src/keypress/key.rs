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

#[cfg(test)]
mod tests {
    use floem::keyboard::{
        Key, KeyCode, KeyLocation, NamedKey, NativeKey, PhysicalKey,
    };

    use super::*;

    fn keyboard_input(
        logical: Key,
        physical: PhysicalKey,
        key_without_modifiers: Key,
        location: KeyLocation,
        repeat: bool,
    ) -> KeyInput {
        KeyInput::Keyboard {
            physical,
            logical,
            location,
            key_without_modifiers,
            repeat,
        }
    }

    #[test]
    fn repeated_modifier_shift_suppressed() {
        let input = keyboard_input(
            Key::Named(NamedKey::Shift),
            PhysicalKey::Code(KeyCode::ShiftLeft),
            Key::Named(NamedKey::Shift),
            KeyLocation::Left,
            true, // repeat
        );
        assert_eq!(input.keymap_key(), None);
    }

    #[test]
    fn repeated_modifier_ctrl_suppressed() {
        let input = keyboard_input(
            Key::Named(NamedKey::Control),
            PhysicalKey::Code(KeyCode::ControlLeft),
            Key::Named(NamedKey::Control),
            KeyLocation::Left,
            true,
        );
        assert_eq!(input.keymap_key(), None);
    }

    #[test]
    fn repeated_modifier_alt_suppressed() {
        let input = keyboard_input(
            Key::Named(NamedKey::Alt),
            PhysicalKey::Code(KeyCode::AltLeft),
            Key::Named(NamedKey::Alt),
            KeyLocation::Left,
            true,
        );
        assert_eq!(input.keymap_key(), None);
    }

    #[test]
    fn repeated_modifier_meta_suppressed() {
        let input = keyboard_input(
            Key::Named(NamedKey::Meta),
            PhysicalKey::Code(KeyCode::Meta),
            Key::Named(NamedKey::Meta),
            KeyLocation::Standard,
            true,
        );
        assert_eq!(input.keymap_key(), None);
    }

    #[test]
    fn non_repeated_modifier_passes_through() {
        let input = keyboard_input(
            Key::Named(NamedKey::Shift),
            PhysicalKey::Code(KeyCode::ShiftLeft),
            Key::Named(NamedKey::Shift),
            KeyLocation::Left,
            false, // not repeat
        );
        let result = input.keymap_key();
        assert!(result.is_some());
        assert_eq!(
            result.unwrap(),
            KeyMapKey::Logical(Key::Named(NamedKey::Shift))
        );
    }

    #[test]
    fn ascii_char_lowercased() {
        let input = keyboard_input(
            Key::Character("A".into()),
            PhysicalKey::Code(KeyCode::KeyA),
            Key::Character("A".into()),
            KeyLocation::Standard,
            false,
        );
        let result = input.keymap_key().unwrap();
        assert_eq!(result, KeyMapKey::Logical(Key::Character("a".into())));
    }

    #[test]
    fn ascii_char_already_lowercase() {
        let input = keyboard_input(
            Key::Character("z".into()),
            PhysicalKey::Code(KeyCode::KeyZ),
            Key::Character("z".into()),
            KeyLocation::Standard,
            false,
        );
        let result = input.keymap_key().unwrap();
        assert_eq!(result, KeyMapKey::Logical(Key::Character("z".into())));
    }

    #[test]
    fn space_char_maps_to_named_space() {
        let input = keyboard_input(
            Key::Character(" ".into()),
            PhysicalKey::Code(KeyCode::Space),
            Key::Character(" ".into()),
            KeyLocation::Standard,
            false,
        );
        let result = input.keymap_key().unwrap();
        assert_eq!(result, KeyMapKey::Logical(Key::Named(NamedKey::Space)));
    }

    #[test]
    fn named_key_passthrough() {
        let input = keyboard_input(
            Key::Named(NamedKey::Enter),
            PhysicalKey::Code(KeyCode::Enter),
            Key::Named(NamedKey::Enter),
            KeyLocation::Standard,
            false,
        );
        let result = input.keymap_key().unwrap();
        assert_eq!(result, KeyMapKey::Logical(Key::Named(NamedKey::Enter)));
    }

    #[test]
    fn non_ascii_uses_physical_key_fallback() {
        // e.g. "ü" on a German keyboard — multi-byte, non-ASCII
        let input = keyboard_input(
            Key::Character("ü".into()),
            PhysicalKey::Code(KeyCode::BracketLeft),
            Key::Character("ü".into()),
            KeyLocation::Standard,
            false,
        );
        let result = input.keymap_key().unwrap();
        assert_eq!(
            result,
            KeyMapKey::Physical(PhysicalKey::Code(KeyCode::BracketLeft))
        );
    }

    #[test]
    fn dead_key_uses_physical_key() {
        let input = keyboard_input(
            Key::Dead(Some('`')),
            PhysicalKey::Code(KeyCode::Backquote),
            Key::Dead(Some('`')),
            KeyLocation::Standard,
            false,
        );
        let result = input.keymap_key().unwrap();
        assert_eq!(
            result,
            KeyMapKey::Physical(PhysicalKey::Code(KeyCode::Backquote))
        );
    }

    #[test]
    fn unidentified_key_uses_physical_key() {
        let input = keyboard_input(
            Key::Unidentified(NativeKey::Unidentified),
            PhysicalKey::Code(KeyCode::F13),
            Key::Unidentified(NativeKey::Unidentified),
            KeyLocation::Standard,
            false,
        );
        let result = input.keymap_key().unwrap();
        assert_eq!(result, KeyMapKey::Physical(PhysicalKey::Code(KeyCode::F13)));
    }

    #[test]
    fn numpad_key_uses_logical_key() {
        let input = keyboard_input(
            Key::Named(NamedKey::Enter),
            PhysicalKey::Code(KeyCode::NumpadEnter),
            Key::Named(NamedKey::Enter),
            KeyLocation::Numpad,
            false,
        );
        let result = input.keymap_key().unwrap();
        // Numpad uses logical key directly (not lowercased/normalized)
        assert_eq!(result, KeyMapKey::Logical(Key::Named(NamedKey::Enter)));
    }

    #[test]
    fn pointer_button_passthrough() {
        use floem::pointer::{MouseButton, PointerButton};

        let input = KeyInput::Pointer(PointerButton::Mouse(MouseButton::X1));
        let result = input.keymap_key().unwrap();
        assert_eq!(
            result,
            KeyMapKey::Pointer(PointerButton::Mouse(MouseButton::X1))
        );
    }

    #[test]
    fn repeated_regular_key_not_suppressed() {
        // Only modifier repeats are suppressed, not regular keys
        let input = keyboard_input(
            Key::Character("a".into()),
            PhysicalKey::Code(KeyCode::KeyA),
            Key::Character("a".into()),
            KeyLocation::Standard,
            true, // repeat
        );
        let result = input.keymap_key();
        assert!(result.is_some());
    }
}
