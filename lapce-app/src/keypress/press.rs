use floem::keyboard::Modifiers;

use super::{key::KeyInput, keymap::KeyMapPress};

/// Represents a single physical key press event with its modifier state.
/// This is the raw event; `keymap_press()` converts it to the canonical
/// `KeyMapPress` form used for keymap lookup.
#[derive(Clone, Debug)]
pub struct KeyPress {
    pub(super) key: KeyInput,
    pub(super) mods: Modifiers,
}

impl KeyPress {
    pub fn keymap_press(&self) -> Option<KeyMapPress> {
        self.key.keymap_key().map(|key| KeyMapPress {
            key,
            mods: self.mods,
        })
    }
}
