#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    File,
    Line,
    Workspace,
    Reference,
    ColorTheme,
    IconTheme,
    Language,
    LineEnding,
}
