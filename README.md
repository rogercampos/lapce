<h1 align="center">
  <a href="https://lapce.dev" target="_blank">
  <img src="extra/images/logo.png" width=200 height=200/><br>
  Lapce
  </a>
</h1>

<h4 align="center">Lightning-fast And Powerful Code Editor</h4>

<div align="center">
  <a href="https://github.com/lapce/lapce/actions/workflows/ci.yml" target="_blank">
    <img src="https://github.com/lapce/lapce/actions/workflows/ci.yml/badge.svg" />
  </a>
  <a href="https://discord.gg/n8tGJ6Rn6D" target="_blank">
    <img src="https://img.shields.io/discord/946858761413328946?logo=discord" />
  </a>
  <a href="https://docs.lapce.dev" target="_blank">
      <img src="https://img.shields.io/static/v1?label=Docs&message=docs.lapce.dev&color=blue" alt="Lapce Docs">
  </a>
</div>
<br/>

Lapce (IPA: /læps/) is written in pure Rust, with a UI in [Floem](https://github.com/lapce/floem). It is designed with [Rope Science](https://xi-editor.io/docs/rope_science_00.html) from the [Xi-Editor](https://github.com/xi-editor/xi-editor), enabling lightning-fast computation, and leverages [wgpu](https://github.com/gfx-rs/wgpu) for rendering. More information about the features of Lapce can be found on the [main website](https://lapce.dev) and user documentation can be found on [GitBook](https://docs.lapce.dev/).

![](https://github.com/lapce/lapce/blob/master/extra/images/screenshot.png?raw=true)

## Features

* **Built-in LSP support** — [Language Server Protocol](https://microsoft.github.io/language-server-protocol/) integration provides intelligent code features such as completion, diagnostics, code actions, hover documentation, go to definition, find references, rename, and more.
* **Modal editing** — First-class Vim-like modal editing (toggleable). Supports normal, insert, and visual modes with motions, operators, marks, inline find, and more.
* **Plugin system** — Extend Lapce with plugins written in any language that compiles to [WASI](https://wasi.dev/) (C, Rust, [AssemblyScript](https://www.assemblyscript.org/)).
* **Command palette** — Quick access to every command, file, symbol, and setting from a single searchable popup.
* **Split editing** — Work on multiple files side by side with vertical and horizontal splits.
* **Customizable** — Themes, keybindings, fonts, and dozens of editor settings are all configurable.

---

## User Guide

### Command Palette

The command palette is the central hub for finding and running any command. Open it with:

| Platform | Shortcut |
|----------|----------|
| macOS | `Cmd+Shift+P` |
| Linux / Windows | `Ctrl+Shift+P` |

Type to fuzzy-search all available commands. The palette also supports prefix modes:

| Prefix | Mode | Shortcut (macOS / Other) |
|--------|------|--------------------------|
| *(none)* | Go to File | `Cmd+P` / `Ctrl+P` |
| `/` | Go to Line | `Ctrl+G` |
| `@` | Go to Symbol in File | `Cmd+Shift+O` / `Ctrl+Shift+O` |
| `#` | Go to Symbol in Workspace | `Cmd+T` / `Ctrl+T` |
| `:` | Command Palette | `Cmd+Shift+P` / `Ctrl+Shift+P` |
| `>` | Open Recent Workspace | — |
| `?` | Palette Help | — |

### Editing

#### Basic Operations

| Action | macOS | Linux / Windows |
|--------|-------|-----------------|
| Undo | `Cmd+Z` | `Ctrl+Z` |
| Redo | `Cmd+Shift+Z` or `Cmd+Y` | `Ctrl+Shift+Z` or `Ctrl+Y` |
| Cut | `Cmd+X` | `Ctrl+X` |
| Copy | `Cmd+C` | `Ctrl+C` |
| Paste | `Cmd+V` | `Ctrl+V` |
| Select All | `Cmd+A` | `Ctrl+A` |
| Delete Line | `Cmd+Shift+K` | — |
| Toggle Line Comment | `Cmd+/` | `Ctrl+/` |
| Indent | `Cmd+]` | `Ctrl+]` |
| Outdent | `Cmd+[` | `Ctrl+[` |
| Move Line Up | `Alt+Up` | `Alt+Up` |
| Move Line Down | `Alt+Down` | `Alt+Down` |
| Duplicate Line Up | `Alt+Shift+Up` | `Alt+Shift+Up` |
| Duplicate Line Down | `Alt+Shift+Down` | `Alt+Shift+Down` |
| New Line Below | `Cmd+Enter` | `Ctrl+Enter` |
| New Line Above | `Cmd+Shift+Enter` | `Ctrl+Shift+Enter` |

#### Word and Line Deletion

| Action | macOS | Linux / Windows |
|--------|-------|-----------------|
| Delete Word Backward | `Alt+Backspace` | `Ctrl+Backspace` |
| Delete Word Forward | `Alt+Delete` | `Ctrl+Delete` |
| Delete to Beginning of Line | `Cmd+Backspace` | — |
| Delete to End of Line | `Ctrl+K` | — |

### Navigation

#### Cursor Movement

| Action | macOS | Linux / Windows |
|--------|-------|-----------------|
| Word Forward | `Alt+Right` | `Ctrl+Right` |
| Word Backward | `Alt+Left` | `Ctrl+Left` |
| Line Start | `Cmd+Left` or `Ctrl+A` | `Home` |
| Line End | `Cmd+Right` or `Ctrl+E` | `End` |
| Document Start | `Cmd+Up` | `Ctrl+Home` |
| Document End | `Cmd+Down` | `Ctrl+End` |
| Page Up | `PageUp` | `PageUp` |
| Page Down | `PageDown` | `PageDown` |
| Match Bracket | `Cmd+Shift+\` | `Ctrl+Shift+\` |

#### Jump History

Navigate between previous cursor locations:

| Action | macOS | Linux / Windows |
|--------|-------|-----------------|
| Jump Back | `Ctrl+-` | `Ctrl+-` |
| Jump Forward | `Ctrl+Shift+-` | `Ctrl+Shift+-` |

#### Error Navigation

| Action | Shortcut |
|--------|----------|
| Next Error | `F8` |
| Previous Error | `Shift+F8` |

### Find and Replace

| Action | macOS | Linux / Windows |
|--------|-------|-----------------|
| Open Find | `Cmd+F` | `Ctrl+F` |
| Find Next | `Enter` (when in find) | `Enter` (when in find) |
| Find Previous | `Shift+Enter` (when in find) | `Shift+Enter` (when in find) |
| Switch to Replace | `Tab` (when in find) | `Tab` (when in find) |
| Close Find | `Escape` | `Escape` |

Find supports regular expressions, case-sensitive matching, and whole-word matching.

### Global Search

Search across all files in the workspace:

| Platform | Shortcut |
|----------|----------|
| macOS | `Cmd+Shift+F` |
| Linux / Windows | `Ctrl+Shift+F` |

Features regex support, case-sensitive and whole-word toggles, and replace across files.

### Language Server Protocol (LSP)

Lapce communicates with language servers to provide intelligent features. These are delivered through plugins that bundle or connect to the appropriate language server for each language.

| Feature | How to Access |
|---------|---------------|
| Completion | Automatic as you type, or `Ctrl+Space` / `Cmd+I` |
| Hover Documentation | Mouse hover over symbol, or `g h` in Vim normal mode |
| Go to Definition | `F12`, or `g d` in Vim normal mode |
| Go to Implementation | Command palette: "Go to Implementation" |
| Find References | Command palette: "Find References" |
| Rename Symbol | `F2` |
| Code Actions / Quick Fixes | `Cmd+.` (macOS) or `Ctrl+.` (Linux/Windows) |
| Signature Help | `Ctrl+Shift+Space` |
| Document Symbols | `Cmd+Shift+O` / `Ctrl+Shift+O` |
| Workspace Symbols | `Cmd+T` / `Ctrl+T` |
| Call Hierarchy | Command palette: "Show Call Hierarchy" |
| Diagnostics | Shown inline (error lens) and in the Problems panel |
| Format on Save | Enable via `editor.format-on-save` setting |
| Inlay Hints | Enable via `editor.enable-inlay-hints` setting |

### Inline Completion

Lapce supports inline completions (ghost text suggestions from LSP-compatible providers):

| Action | Shortcut |
|--------|----------|
| Accept | `Tab` |
| Cancel | `Escape` |
| Next suggestion | `Alt+]` |
| Previous suggestion | `Alt+[` |
| Manually invoke | `Alt+\` |

Enable/disable with the `editor.enable-inline-completion` setting.

### Modal Editing (Vim Mode)

Modal editing is disabled by default. Enable it via:
- Command palette: "Enable Modal Editing"
- Setting: `core.modal = true`

When enabled, the editor operates in three modes:

**Normal mode** — navigate and operate on text with Vim keybindings:
- Movement: `h/j/k/l`, `w/b/e`, `0/$`, `gg/G`, `f/F` (inline find), `%` (bracket match)
- Operators: `d` (delete), `y` (yank), `c` (change), `>/<` (indent/outdent)
- Actions: `p` (paste), `u` (undo), `Ctrl+R` (redo), `J` (join lines)
- Marks: `m` (create mark), `'` (go to mark)
- Visual modes: `v` (character), `V` (linewise), `Ctrl+V` (block)
- Window splits: `Ctrl+W` followed by `h/j/k/l/s/v/c/x`

**Insert mode** — type text normally. Enter with `i`, `a`, `A`, `I`, `o`, `O`, `s`, `S`, `c`.

**Visual mode** — select text for operations. Enter with `v`, `V`, or `Ctrl+V`.

Return to normal mode with `Escape`, `Ctrl+C`, or `Ctrl+[`.

The setting `editor.modal-mode-relative-line-numbers` (default: true) shows relative line numbers in normal mode.

### Split Editor

Work on multiple files side by side:

| Action | macOS | Linux / Windows | Vim |
|--------|-------|-----------------|-----|
| Split Vertical | `Cmd+\` | `Ctrl+\` | `Ctrl+W v` |
| Split Horizontal | — | — | `Ctrl+W s` |
| Navigate Splits | — | — | `Ctrl+W h/j/k/l` |
| Close Split | `Cmd+W` | `Ctrl+W` | `Ctrl+W c` |
| Exchange Splits | — | — | `Ctrl+W x` |

### Editor Tabs

| Action | Shortcut |
|--------|----------|
| Next Tab | `Ctrl+Tab` |
| Previous Tab | `Ctrl+Shift+Tab` |
| Close Tab | `Cmd+W` (macOS) / `Ctrl+W` (Linux/Windows) |

### Panels

Lapce has a panel system with panels positioned on the left, right, or bottom:

| Panel | Default Position | Toggle Shortcut (macOS / Other) |
|-------|-----------------|----------------------------------|
| File Explorer | Left | `Cmd+Shift+E` / `Ctrl+Shift+E` |
| Plugins | Left | `Cmd+Shift+X` / `Ctrl+Shift+X` |
| Global Search | Bottom | `Cmd+Shift+F` / `Ctrl+Shift+F` |
| Problems | Bottom | `Cmd+Shift+M` / `Ctrl+Shift+M` |
| Call Hierarchy | Bottom | — |
| Document Symbol | Right | — |
| References | Bottom | — |
| Implementation | Bottom | — |

Toggle panel visibility with:
- "Toggle Left Panel", "Toggle Right Panel", "Toggle Bottom Panel" commands.

### File Explorer

The file explorer panel lets you browse, create, rename, delete, and duplicate files and folders.

- Single-click or double-click to open files (configurable via `core.file-explorer-double-click`)
- "Open Editors" section shows all currently open files (toggle via `ui.open-editors-visible`)
- Exclude files with glob patterns via `editor.files-exclude`
- Reveal current file: command palette "Reveal Active File in File Explorer"
- Reveal in system explorer: command palette "Reveal in Finder" (macOS) / "Reveal in System File Explorer"

### Plugins

Plugins extend Lapce with language support, themes, and other features. They are written in languages that compile to WASI.

- Browse and install plugins from the Plugin panel (`Cmd+Shift+X` / `Ctrl+Shift+X`)
- Plugins can provide: LSP servers, syntax highlighting, code formatting, themes, and custom commands
- Auto-reload plugins on config change: `core.auto-reload-plugin`

### Themes

Lapce ships with built-in dark and light themes. Additional themes can be installed as plugins.

- Change color theme: command palette "Change Color Theme"
- Change icon theme: command palette "Change Icon Theme"
- Export current theme: command palette "Export current settings to a theme file"
- Install theme file: command palette "Install current theme file"
- Edit theme colors directly: command palette "Open Theme Color Settings"

### Code Glance (Minimap)

A miniature overview of your code. Toggle with:
- `Cmd+E` (macOS) / `Ctrl+E` (Linux/Windows)
- `Space` in Vim normal mode
- Configurable font size via `editor.code-glance-font-size`

### Breadcrumbs

Navigation breadcrumbs show the file path and code structure context at the top of the editor. Toggle with `editor.show-bread-crumbs` setting.

### Sticky Header

When scrolling, the editor can show the current code context (function, class, etc.) pinned at the top. Toggle with `editor.sticky-header` setting.

### Error Lens

Diagnostics can be displayed inline at the end of the line where they occur. Configurable settings:
- `editor.enable-error-lens` — enable/disable (default: true)
- `editor.only-render-error-styling` — show only coloring, no message text (default: true)
- `editor.error-lens-end-of-line` — extend to end of view line (default: true)
- `editor.error-lens-multiline` — allow multi-line display (default: false)

### Snippet Support

Lapce supports LSP snippet syntax with tabstops and placeholders:
- `Tab` — jump to next placeholder
- `Shift+Tab` — jump to previous placeholder
- `Escape` — exit snippet mode

### Syntax Selection

Lapce supports expanding and contracting the selection based on the syntax tree:
- `Ctrl+Shift+Up` — expand selection to next syntax node
- `Ctrl+Shift+Down` — contract selection to previous syntax node

---

## Configuration

### Settings File

Open the settings UI with `Cmd+,` (macOS) / `Ctrl+,` (Linux/Windows), or edit the TOML file directly:
- Command palette: "Open Settings File"
- Command palette: "Open Settings Directory"

### Key Settings Reference

#### Core Settings (`[core]`)

| Setting | Default | Description |
|---------|---------|-------------|
| `modal` | `false` | Enable Vim-like modal editing |
| `color-theme` | `"Lapce Dark"` | Color theme name |
| `icon-theme` | `"Lapce Codicons"` | Icon theme name |
| `custom-titlebar` | `true` | Use custom titlebar (Linux/BSD/Windows) |
| `file-explorer-double-click` | `false` | Require double-click to open files |
| `auto-reload-plugin` | `false` | Auto-reload plugins on config change |

#### Editor Settings (`[editor]`)

| Setting | Default | Description |
|---------|---------|-------------|
| `font-family` | `"monospace"` | Editor font family |
| `font-size` | `13` | Editor font size (6-32) |
| `line-height` | `1.5` | Line height (multiplier if < 5.0, absolute pixels otherwise) |
| `tab-width` | `4` | Tab width in spaces |
| `smart-tab` | `true` | Auto-detect indentation from file |
| `show-tab` | `true` | Show editor tabs |
| `show-bread-crumbs` | `true` | Show navigation breadcrumbs |
| `scroll-beyond-last-line` | `true` | Allow scrolling past end of file |
| `cursor-surrounding-lines` | `1` | Minimum visible lines around cursor |
| `wrap-style` | `"editor-width"` | Wrapping mode: `none`, `editor-width`, `wrap-width` |
| `wrap-width` | `600` | Pixel width for `wrap-width` mode |
| `sticky-header` | `true` | Show code context at top of editor |
| `completion-width` | `600` | Completion popup width in pixels |
| `completion-show-documentation` | `true` | Show documentation in completion popup |
| `show-signature` | `true` | Show function signature while typing |
| `auto-closing-matching-pairs` | `true` | Auto-close brackets and quotes |
| `auto-surround` | `true` | Auto-surround selection with brackets/quotes |
| `hover-delay` | `300` | Milliseconds before hover appears |
| `modal-mode-relative-line-numbers` | `true` | Relative line numbers in Vim mode |
| `format-on-save` | `false` | Format on save (requires language server) |
| `normalize-line-endings` | `true` | Convert line endings on save |
| `highlight-matching-brackets` | `true` | Highlight matching bracket pairs |
| `highlight-scope-lines` | `false` | Highlight scope boundary lines |
| `enable-inlay-hints` | `true` | Show inlay hints from language server |
| `enable-error-lens` | `true` | Show diagnostics inline |
| `enable-completion-lens` | `false` | Show completion as phantom text |
| `enable-inline-completion` | `true` | Enable inline ghost-text completions |
| `blink-interval` | `500` | Cursor blink interval in ms (0 to disable) |
| `render-whitespace` | `"none"` | Render whitespace: `none`, `all`, `boundary`, `trailing` |
| `show-indent-guide` | `true` | Show indentation guide lines |
| `autosave-interval` | `0` | Auto-save delay in ms (0 to disable) |
| `atomic-soft-tabs` | `false` | Treat soft tabs as hard tabs for cursor movement |
| `double-click` | `"single"` | Click behavior: `single`, `file`, `all` |
| `move-focus-while-search` | `true` | Move editor focus while typing in search |
| `diff-context-lines` | `3` | Lines of context in diff view (-1 for all) |
| `bracket-pair-colorization` | `false` | Colorize matching bracket pairs |
| `files-exclude` | `"**/{.git,.svn,...}"` | Glob patterns to exclude from file explorer |

#### UI Settings (`[ui]`)

| Setting | Default | Description |
|---------|---------|-------------|
| `scale` | `1.0` | UI scale factor (0.1 - 4.0) |
| `font-family` | `""` | UI font (empty = system default) |
| `font-size` | `13` | UI font size (6-32) |
| `icon-size` | `0` | Icon size (0 = use font-size) |
| `header-height` | `36` | Panel/tab header height |
| `status-height` | `25` | Status bar height |
| `tab-min-width` | `100` | Minimum editor tab width |
| `scroll-width` | `10` | Scrollbar width |
| `drop-shadow-width` | `0` | Popup drop shadow width |
| `palette-width` | `500` | Command palette width |
| `tab-close-button` | `"Right"` | Close button position: `Left`, `Right`, `Off` |
| `tab-separator-height` | `"Content"` | Tab separator: `Content` or `Full` |
| `open-editors-visible` | `true` | Show "Open Editors" in file explorer |
| `list-line-height` | `25` | List item height |

### Keybindings

Customize keybindings by opening the keybindings UI (`Cmd+K Cmd+S` on macOS, `Ctrl+K Ctrl+S` on Linux/Windows) or editing the file directly via "Open Keyboard Shortcuts File" in the command palette.

### Zoom

| Action | macOS | Linux / Windows |
|--------|-------|-----------------|
| Zoom In | `Cmd+=` | `Ctrl+=` |
| Zoom Out | `Cmd+-` | `Ctrl+-` |
| Reset Zoom | Command palette: "Reset Zoom" | Command palette: "Reset Zoom" |

### macOS-Specific

- **Install to PATH**: Command palette "Install Lapce to PATH" — adds `lapce` CLI to your system path.
- **Uninstall from PATH**: Command palette "Uninstall Lapce from PATH".

---

## Installation

You can find pre-built releases for Windows, Linux and macOS [here](https://github.com/lapce/lapce/releases), or [installing with a package manager](docs/installing-with-package-manager.md).
If you'd like to compile from source, you can find the [guide](docs/building-from-source.md).

## Contributing

Guidelines for contributing to Lapce can be found in [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Feedback & Contact

The most popular place for Lapce developers and users is on the [Discord server](https://discord.gg/n8tGJ6Rn6D).

Or, join the discussion on [Reddit](https://www.reddit.com/r/lapce/) where we are just getting started.

There is also a [Matrix Space](https://matrix.to/#/#lapce-editor:matrix.org), which is linked to the content from the Discord server.

## License

Lapce is released under the Apache License Version 2, which is an open source license. You may contribute to this project, or use the code as you please as long as you adhere to its conditions. You can find a copy of the license text here: [`LICENSE`](LICENSE).
