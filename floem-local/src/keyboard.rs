use bitflags::bitflags;
pub use winit::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, NativeKey, PhysicalKey,
};
#[cfg(not(any(
    target_arch = "wasm32",
    target_os = "ios",
    target_os = "android"
)))]
pub use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEvent {
    pub key: winit::event::KeyEvent,
    pub modifiers: Modifiers,
}

bitflags! {
    /// Represents the current state of the keyboard modifiers
    ///
    /// Each flag represents a modifier and is set if this modifier is active.
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct Modifiers: u32 {
        /// The "shift" key.
        const SHIFT = 0b100;
        /// The "control" key.
        const CONTROL = 0b100 << 3;
        /// The "alt" key.
        const ALT = 0b100 << 6;
        /// This is the "windows" key on PC and "command" key on Mac.
        const META = 0b100 << 9;
        /// The "altgr" key.
        const ALTGR = 0b100 << 12;
    }
}

impl Modifiers {
    /// Returns `true` if the shift key is pressed.
    pub fn shift(&self) -> bool {
        self.intersects(Self::SHIFT)
    }
    /// Returns `true` if the control key is pressed.
    pub fn control(&self) -> bool {
        self.intersects(Self::CONTROL)
    }
    /// Returns `true` if the alt key is pressed.
    pub fn alt(&self) -> bool {
        self.intersects(Self::ALT)
    }
    /// Returns `true` if the meta key is pressed.
    pub fn meta(&self) -> bool {
        self.intersects(Self::META)
    }
    /// Returns `true` if the altgr key is pressed.
    pub fn altgr(&self) -> bool {
        self.intersects(Self::ALTGR)
    }
}

impl From<ModifiersState> for Modifiers {
    fn from(value: ModifiersState) -> Self {
        let mut modifiers = Modifiers::empty();
        if value.shift_key() {
            modifiers.set(Modifiers::SHIFT, true);
        }
        if value.alt_key() {
            modifiers.set(Modifiers::ALT, true);
        }
        if value.control_key() {
            modifiers.set(Modifiers::CONTROL, true);
        }
        if value.super_key() {
            modifiers.set(Modifiers::META, true);
        }
        modifiers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Modifiers methods ──

    #[test]
    fn default_modifiers_is_empty() {
        let m = Modifiers::default();
        assert!(!m.shift());
        assert!(!m.control());
        assert!(!m.alt());
        assert!(!m.meta());
        assert!(!m.altgr());
    }

    #[test]
    fn shift_flag() {
        let m = Modifiers::SHIFT;
        assert!(m.shift());
        assert!(!m.control());
        assert!(!m.alt());
        assert!(!m.meta());
        assert!(!m.altgr());
    }

    #[test]
    fn control_flag() {
        let m = Modifiers::CONTROL;
        assert!(!m.shift());
        assert!(m.control());
        assert!(!m.alt());
        assert!(!m.meta());
        assert!(!m.altgr());
    }

    #[test]
    fn alt_flag() {
        let m = Modifiers::ALT;
        assert!(!m.shift());
        assert!(!m.control());
        assert!(m.alt());
        assert!(!m.meta());
        assert!(!m.altgr());
    }

    #[test]
    fn meta_flag() {
        let m = Modifiers::META;
        assert!(!m.shift());
        assert!(!m.control());
        assert!(!m.alt());
        assert!(m.meta());
        assert!(!m.altgr());
    }

    #[test]
    fn altgr_flag() {
        let m = Modifiers::ALTGR;
        assert!(!m.shift());
        assert!(!m.control());
        assert!(!m.alt());
        assert!(!m.meta());
        assert!(m.altgr());
    }

    #[test]
    fn combined_modifiers() {
        let m = Modifiers::SHIFT | Modifiers::CONTROL | Modifiers::ALT;
        assert!(m.shift());
        assert!(m.control());
        assert!(m.alt());
        assert!(!m.meta());
        assert!(!m.altgr());
    }

    #[test]
    fn all_modifiers() {
        let m = Modifiers::SHIFT
            | Modifiers::CONTROL
            | Modifiers::ALT
            | Modifiers::META
            | Modifiers::ALTGR;
        assert!(m.shift());
        assert!(m.control());
        assert!(m.alt());
        assert!(m.meta());
        assert!(m.altgr());
    }

    // ── From<ModifiersState> ──

    #[test]
    fn from_modifiers_state_empty() {
        let state = ModifiersState::empty();
        let m: Modifiers = state.into();
        assert_eq!(m, Modifiers::empty());
    }

    #[test]
    fn from_modifiers_state_shift() {
        let state = ModifiersState::SHIFT;
        let m: Modifiers = state.into();
        assert!(m.shift());
        assert!(!m.control());
        assert!(!m.alt());
        assert!(!m.meta());
    }

    #[test]
    fn from_modifiers_state_control() {
        let state = ModifiersState::CONTROL;
        let m: Modifiers = state.into();
        assert!(!m.shift());
        assert!(m.control());
        assert!(!m.alt());
        assert!(!m.meta());
    }

    #[test]
    fn from_modifiers_state_alt() {
        let state = ModifiersState::ALT;
        let m: Modifiers = state.into();
        assert!(!m.shift());
        assert!(!m.control());
        assert!(m.alt());
        assert!(!m.meta());
    }

    #[test]
    fn from_modifiers_state_super_maps_to_meta() {
        let state = ModifiersState::SUPER;
        let m: Modifiers = state.into();
        assert!(!m.shift());
        assert!(!m.control());
        assert!(!m.alt());
        assert!(m.meta());
    }

    #[test]
    fn from_modifiers_state_combined() {
        let state =
            ModifiersState::SHIFT | ModifiersState::CONTROL | ModifiersState::ALT;
        let m: Modifiers = state.into();
        assert!(m.shift());
        assert!(m.control());
        assert!(m.alt());
        assert!(!m.meta());
    }

    #[test]
    fn from_modifiers_state_all() {
        let state = ModifiersState::SHIFT
            | ModifiersState::CONTROL
            | ModifiersState::ALT
            | ModifiersState::SUPER;
        let m: Modifiers = state.into();
        assert!(m.shift());
        assert!(m.control());
        assert!(m.alt());
        assert!(m.meta());
    }

    #[test]
    fn altgr_not_in_modifiers_state() {
        // ModifiersState doesn't have an ALTGR flag, so converting
        // from any ModifiersState never sets ALTGR
        let state = ModifiersState::all();
        let m: Modifiers = state.into();
        assert!(!m.altgr());
    }

    // ── Bitflag properties ──

    #[test]
    fn modifier_bits_are_distinct() {
        // Ensure each modifier has unique bit patterns
        let flags = [
            Modifiers::SHIFT,
            Modifiers::CONTROL,
            Modifiers::ALT,
            Modifiers::META,
            Modifiers::ALTGR,
        ];
        for (i, a) in flags.iter().enumerate() {
            for (j, b) in flags.iter().enumerate() {
                if i != j {
                    assert!(
                        !a.intersects(*b),
                        "{:?} should not intersect {:?}",
                        a,
                        b
                    );
                }
            }
        }
    }
}
