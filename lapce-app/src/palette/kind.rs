#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    File,
    Line,
    Command,
    Workspace,
    Reference,
    DocumentSymbol,
    WorkspaceSymbol,
    ColorTheme,
    IconTheme,
    Language,
    LineEnding,
}
