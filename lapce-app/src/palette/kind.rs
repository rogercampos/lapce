#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    File,
    Line,
    Workspace,
    Reference,
    Language,
    LineEnding,
}
