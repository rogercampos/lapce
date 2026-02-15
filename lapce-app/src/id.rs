use floem::views::editor::id::Id;

/// Defines a newtype wrapper around floem's `Id`, providing compile-time
/// safety so that different ID kinds cannot be accidentally interchanged.
macro_rules! define_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(Id);

        impl $name {
            pub fn next() -> Self {
                Self(Id::next())
            }

            pub fn to_raw(self) -> u64 {
                self.0.to_raw()
            }
        }
    };
}

define_id!(SplitId);
define_id!(WorkspaceId);
define_id!(EditorTabId);
define_id!(SettingsId);
define_id!(KeymapId);
define_id!(ThemeColorSettingsId);
