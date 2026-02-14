# Lapce Architecture

This document describes the architecture, design decisions, and organization of the Lapce code editor. It is intended as a foundational reference for developers working on the project.

## Table of Contents

- [Overview](#overview)
- [Crate Structure](#crate-structure)
- [Two-Process Architecture](#two-process-architecture)
- [App State Hierarchy](#app-state-hierarchy)
- [Initialization Flow](#initialization-flow)
- [Layout and Rendering](#layout-and-rendering)
- [Editor System](#editor-system)
- [Document Model](#document-model)
- [Split Tree Architecture](#split-tree-architecture)
- [Command System](#command-system)
- [Focus and Keyboard Routing](#focus-and-keyboard-routing)
- [Keypress Matching](#keypress-matching)
- [Configuration System](#configuration-system)
- [Theme System](#theme-system)
- [Panel System](#panel-system)
- [Search System](#search-system)
- [Plugin System](#plugin-system)
- [Language Support and Syntax Highlighting](#language-support-and-syntax-highlighting)
- [Persistence Layer](#persistence-layer)
- [RPC Protocol](#rpc-protocol)
- [Concurrency Model](#concurrency-model)

---

## Overview

Lapce is a modal code editor built in Rust using the [Floem](https://github.com/lapce/floem) reactive UI framework. The project is organized as a Cargo workspace with four crates that separate concerns across UI, backend processing, messaging, and core editor logic.

**Key design principles:**
- **Reactive UI**: All state uses Floem reactive signals (`RwSignal<T>`, `ReadSignal<T>`, `Memo<T>`). UI re-renders automatically when signals change.
- **Two-process separation**: UI and backend work (LSP, plugins, file I/O, search) run on separate threads communicating via channels.
- **Immutable data structures**: Uses the `im` crate's persistent data structures for efficient structural sharing in the reactive signal system.
- **Plugin extensibility**: Language support, themes, and commands can be extended through WASI-based plugins.

---

## Crate Structure

```
lapce/
├── lapce-app/     # UI application (~37,000 lines)
│                  # Floem framework, all views and state management
│
├── lapce-proxy/   # Backend process (~7,000 lines)
│                  # LSP clients, WASI plugin runtime, file I/O, search
│
├── lapce-rpc/     # RPC message types (~2,400 lines)
│                  # Shared between app and proxy, defines the protocol
│
└── lapce-core/    # Core editor logic (~5,200 lines)
                   # Language definitions, syntax highlighting, encoding
```

**Dependency flow:**

```
lapce-app → lapce-proxy → lapce-rpc → (lsp-types, serde, crossbeam-channel)
                            ↑
lapce-app → lapce-core ─────┘
              ↑
              └── floem_editor_core (re-exported as lapce_core::*)
              └── tree-sitter, libloading (grammar loading)
              └── xi-rope (text storage)
```

### lapce-core

Contains non-UI editor logic shared by both the app and proxy:

- **Language detection** (`language.rs`): Maps file extensions/names to `LapceLanguage` enum variants (~100 languages). Each variant indexes into a `LANGUAGES` array of `SyntaxProperties` containing indent rules, comment tokens, and tree-sitter configuration.
- **Syntax highlighting** (`syntax/`): Full tree-sitter integration with multi-layer injection support (e.g., JS inside HTML), incremental parsing, bracket colorization, and highlight iteration.
- **Code lens** (`lens.rs`): A balanced tree data structure (built on xi-rope's tree infrastructure) mapping line numbers to variable pixel heights for O(log n) height-to-line lookups.
- **Encoding** (`encoding.rs`): UTF-8 ↔ UTF-16 offset conversion for LSP interop.
- **Directory management** (`directory.rs`): Filesystem paths for configs, plugins, grammars, themes, logs.
- **Build metadata** (`build.rs` + `meta.rs`): Version, release type, and app name generated at compile time.
- **Re-exports** (`lib.rs`): Everything from `floem_editor_core` (rope, buffer, commands, cursor).

### lapce-rpc

Defines the communication protocol between UI and proxy. Deliberately kept lightweight (no tree-sitter, no UI framework dependencies):

- **Message types**: `ProxyRequest`/`ProxyNotification`/`ProxyResponse` (UI → Proxy) and `CoreNotification` (Proxy → UI).
- **Transport**: JSON-over-newline-delimited stdio (`stdio.rs`) for potential future separate-process mode.
- **Shared data structures**: `FileNodeItem` (file tree), `BufferId`, `VoltID`/`VoltInfo`/`VoltMetadata` (plugins), `Style`/`LineStyle`/`SemanticStyles` (highlighting).
- **RPC handler infrastructure**: `ProxyRpcHandler` and `CoreRpcHandler` provide typed channels with request/response correlation via pending maps.

### lapce-proxy

The backend "process" (currently runs as in-process threads for local workspaces):

- **Dispatcher** (`dispatch.rs`): Central message router implementing `ProxyHandler`. Sequential processing prevents data races on buffer state.
- **Plugin system** (`plugin/`): Three-layer architecture — catalog (lifecycle), PSP (per-plugin RPC), and implementations (LSP process, WASI runtime).
- **Buffer management** (`buffer.rs`): Rope-based document storage with revision tracking and UTF-8/UTF-16 conversion.
- **File watching** (`watcher.rs`): Token-based routing with deduplication and debouncing via the `notify` crate.

### lapce-app

The UI application (~80 source files), organized into:

- **App entry and windows**: `app.rs`, `window.rs`, `bin/lapce.rs`
- **Editor subsystem**: `editor.rs`, `editor/view.rs`, `editor/gutter.rs`, `doc.rs`, `main_split.rs`, `editor_tab.rs`
- **State management**: `workspace_data.rs`, `proxy.rs`, `db.rs`
- **Configuration**: `config.rs`, `config/*`, `keypress/*`, `keymap.rs`, `settings.rs`
- **UI features**: `palette.rs`, `completion.rs`, `code_action.rs`, `rename.rs`, `text_input.rs`, `find.rs`, `snippet.rs`
- **Panels and search**: `panel/*`, `global_search.rs`, `search_modal.rs`, `recent_files.rs`, `file_explorer/*`
- **Commands**: `command.rs` (command definitions and registry)

---

## Two-Process Architecture

The app runs two sets of threads communicating via channels:

```
┌─────────────┐  ProxyRequest/Notification  ┌─────────────┐
│  lapce-app  │ ──────────────────────────→ │ lapce-proxy  │
│   (UI)      │ ←────────────────────────── │  (backend)   │
└─────────────┘     CoreNotification        └─────────────┘
```

**App threads**: UI rendering, editor state, user input (Floem reactive framework).

**Proxy threads**: LSP clients, WASI plugin runtime, file watching, global search, buffer management.

Despite the architecture name, local workspaces run the proxy as threads within the same process (the separate-process mode was for remote development, now removed). The `bin/lapce-proxy.rs` entry point is only used for the CLI "open file in existing instance" workflow.

Communication uses `crossbeam_channel::unbounded()`. The proxy's `CoreNotification` messages are bridged to Floem's reactive system via `create_signal_from_channel()`, which returns a `ReadSignal` that an effect subscribes to for processing.

---

## App State Hierarchy

```
AppData
└── WindowData (one per OS window)
    └── WorkspaceData (one per workspace tab)
        ├── MainSplitData (recursive editor splits)
        │   ├── SplitData { children, direction }
        │   └── EditorTabData → EditorData → Doc
        ├── PanelData (file explorer, search, problems, etc.)
        ├── PaletteData (command palette)
        ├── PluginData
        ├── GlobalSearchData
        ├── SearchModalData
        ├── RecentFilesData
        ├── FileExplorerData
        └── CommonData (config, proxy handle, diagnostics, shared signals)
```

- **AppData** (`app.rs`): Holds all windows, global config, file watcher, tracing handle.
- **WindowData** (`window.rs`): Per-OS-window state with a vector of workspace tabs and an active index.
- **WorkspaceData** (`workspace_data.rs`): The central orchestrator for a workspace. Holds all sub-components, wires up command listeners, processes proxy notifications.
- **CommonData**: Shared across all components in a workspace — config signal, proxy RPC handle, focus signal, command listeners.

All state uses Floem reactive signals. Key pattern: `signal.get()` (tracked, triggers re-render) vs `signal.get_untracked()` (no subscription). Using tracked gets in non-view code can cause performance issues.

---

## Initialization Flow

The startup sequence (from `main()` through event loop):

1. **CLI parsing**: Clap-based parser handles `--new`, `--wait`, `--plugin-path`, and positional path arguments.
2. **Panic hook + logging**: Custom panic handler with tracing. Dual-output subscriber: file logger (daily-rotated) + console logger.
3. **Vendored fonts**: Loads DejaVu Sans and DejaVu Sans Mono from embedded byte arrays.
4. **Shell environment**: When not launched from terminal (GUI launch), spawns a login shell to capture `printenv` output for PATH.
5. **Process re-spawn**: When `--wait` is NOT set, re-spawns with `--wait` and exits immediately, unblocking the terminal.
6. **Single-instance check**: Attempts to connect to an existing Lapce instance via local socket. If successful, sends paths to open and exits.
7. **Core initialization**: Cleans up old updates, creates DB, sets up file watchers, installs bundled plugins, loads config.
8. **Window creation**: Restores from DB or creates new windows based on CLI arguments.
9. **Background threads**: Config watcher, grammar updater (checks GitHub for tree-sitter releases), update checker, local socket listener.
10. **Event loop**: `app.run()` enters the Floem event loop.

---

## Layout and Rendering

### Workbench Layout

The `workbench()` function in `app.rs` defines the main editor area:

```
workbench (vertical flex column):
  |-- horizontal row (flex-grow: 1):
  |   |-- panel_container_view(Left)     -- file explorer
  |   |-- main_split()                   -- recursive editor split tree
  |   |-- panel_container_view(Right)    -- (empty by default)
  |
  |-- panel_container_view(Bottom)       -- search, problems
```

### Full Workspace View

The workspace view layers floating elements on top:

```
workspace_view (layered stack):
  Base: title bar + workbench + status bar

  Floating overlays (z-order):
    completion → hover → code_action → rename → palette
    → search_modal → recent_files → about → plugin → alert
```

If no folder is open, shows `empty_workspace_view()` — a centered "Open Folder" button.

### Exclusive Popup Pattern

Floating modals (search, recent files, about) use the reusable `exclusive_popup()` function:

```rust
pub fn exclusive_popup(config, visibility, on_close, content) -> impl View
```

Provides: dimmed overlay, click-outside-to-close, centered content, focus integration. The content is responsible for its own styling. Each popup has a corresponding `Focus` variant for keyboard routing.

### Title Bar

Fixed 37px height, three sections:
- **Left**: macOS traffic-light spacer OR logo + menu (non-macOS)
- **Center**: Drag area for window movement
- **Right**: Settings gear (with context menu), optional update badge, window controls

### Status Bar

Fixed height from config, three sections:
- **Left**: Error/warning counts, LSP progress messages
- **Center**: Panel toggle buttons (sidebar controls)
- **Right**: Cursor position, line ending, language mode (all clickable for palette interaction)

---

## Editor System

### EditorData

`EditorData` (`editor.rs`) is the per-editor-instance data structure, wrapping floem's `Editor` with Lapce-specific concerns:

- **Snippet tracking**: Maintains tab-stop positions from LSP snippet completions, updated via `Transformer` on buffer edits.
- **Find state**: Per-editor find/replace bar and single-character inline find (vim f/t commands).
- **Kind**: `Normal` vs `Preview`. Preview editors skip sticky headers and don't steal focus on click.
- **Shared state**: `Rc<CommonData>` — completion, hover, inline completion, proxy, config, focus.

**Clone semantics**: `EditorData` is cheaply cloneable (all fields are signals or `Rc`). The `copy()` method creates a new editor sharing the same `Doc` but with independent cursor/viewport.

### Command Processing Pipeline

1. **Keybinding resolution** → maps key events to `LapceCommand` with `when` conditions
2. **`EditorData::run_command`** → dispatches by `CommandKind`:
   - `Edit` → `Doc::do_edit` → `Buffer::edit`
   - `Move` → `movement::move_cursor`
   - `Scroll` → `Editor::page_move`/`scroll`
   - `Focus` → split, completion, goto-definition, save, rename, etc.
   - `Workbench` → returns `CommandExecuted::No` (handled at workspace tab level)
3. **Find bar intercept**: When `find_focus` is true, commands are forwarded to the find/replace editor.
4. **Floem bridge**: `Doc` implements floem's `Document` trait and delegates back to `EditorData`.

### Editor Rendering Pipeline

`EditorView::paint()` (`editor/view.rs`) renders layers in order:

1. Current line highlight
2. Selection rectangles (delegated to floem)
3. Find result outlines
4. Bracket highlights + scope lines
5. Text + phantom text (delegated to floem)
6. Sticky headers (pinned scope headers at top)
7. Scroll bar

**Sticky headers**: Computed in an effect watching viewport, buffer rev, and screen lines. The algorithm finds enclosing syntax scopes and determines push-up scroll animation at scope boundaries.

### Gutter

`EditorGutterView` (`editor/gutter.rs`) is a custom `View` painting line numbers. Additional overlays for code lens icons, code action lightbulbs, and (future) folding range indicators are layered on top.

---

## Document Model

`Doc` (`doc.rs`) is the document model — one per file, shared via `Rc` across all editors viewing the same file:

- **Buffer** (`RwSignal<Buffer>`): Rope-based text buffer from xi-rope.
- **Dual styling**: Semantic styles (LSP semantic tokens) take priority over tree-sitter syntax styles. Both stored as `Spans<Style>` and shifted on edits.
- **Phantom text**: Virtual text (inlay hints, error lens, completion lens, inline completion, preedit) assembled per-line. Affinity heuristic determines placement relative to surrounding characters.
- **Cache invalidation**: `cache_rev` signal is incremented when visual representation changes (even without text changes, e.g., new inlay hints).
- **Background processing**: Syntax parsing runs on rayon threads with cancellation via `AtomicUsize`. Find operations also run on rayon.

### Doc Constructors

Three constructor paths: `new()` (from file), `new_content()` (scratch/local), `new_history()` (for diff view). All share the same field structure but differ in initialization (loaded flag, content source, syntax detection).

---

## Split Tree Architecture

The editor area uses a **recursive tree** of splits and editor tab panes, managed by `MainSplitData` (`main_split.rs`):

```
Root Split (SplitData)
  |-- child 0: EditorTab (tabs: file1.rs, file2.rs)  [leaf]
  |-- child 1: Split (nested, Horizontal)              [interior]
       |-- child 0: EditorTab (tabs: file3.rs)
       |-- child 1: EditorTab (tabs: file4.rs)
```

### Design Decisions

1. **Flat storage with ID references**: All nodes stored in flat `im::HashMap` maps (`splits`, `editor_tabs`, `editors`) keyed by ID. Tree structure encoded via parent/child ID fields. Enables O(1) lookup and efficient signal-based reactivity.

2. **Immutable data structures**: `im::HashMap` provides structural sharing — updating one entry creates a new map sharing most structure with the old one.

3. **Proportional sizing**: Each split child has an `RwSignal<f64>` weight. Weights reset to 1.0 on split/close for equal distribution.

4. **Geometry-based focus navigation**: `split_move()` uses physical screen coordinates (layout_rect + window_origin) rather than tree traversal to find adjacent panes. Simpler than tree-aware navigation, works regardless of nesting depth.

5. **Tree collapse**: When a split has only one child left, the child is promoted to replace the split, preventing unnecessary nesting.

6. **Document sharing**: The `docs` map caches `Rc<Doc>` by path. Multiple editors can share the same `Doc`.

7. **Dual navigation history**: Global (`MainSplitData.locations`) for cross-split jumps; per-tab (`EditorTabData.locations`) for local back/forward.

### Tab Reuse Policy

`get_editor_tab_child()` implements sophisticated tab reuse:
- With `show_tab` disabled: reuses any pristine editor or one showing the same path.
- With `show_tab` enabled: searches for existing tabs across all editor tabs.
- Can reuse editors by swapping documents (preserves editor state).

---

## Command System

Commands are defined in `command.rs`:

- **`LapceCommand`**: Wraps a `CommandKind` + optional JSON data. The unified command type.
- **`CommandKind`**: 7-variant enum unifying all command families:
  - `Workbench(LapceWorkbenchCommand)` — Lapce-specific (~50 variants)
  - `Edit`/`Move`/`Scroll`/`Focus`/`MotionMode`/`MultiSelection` — from `floem_editor_core`
- **`InternalCommand`**: ~40 variants with rich data payloads. Not exposed in palette/keybindings.
- **`WindowCommand`**: SetWorkspace, NewWindow, CloseWindow.

`LapceWorkbenchCommand` uses strum derive macros: `#[strum(serialize = "...")]` for keybinding matching, `#[strum(message = "...")]` for palette display names. Commands without a message don't appear in the palette.

### Command Dispatch

`WorkspaceData` uses three listener channels:
- `lapce_command`: delegates to workbench_command or active editor
- `workbench_command`: UI-level actions
- `internal_command`: implementation-detail actions

---

## Focus and Keyboard Routing

### Two-Level Focus System

1. **App-level focus** (`Focus` enum in `workspace_data.rs`): Determines which component receives keyboard events. Set via `common.focus.set(Focus::Variant)`.
2. **Floem-level focus**: Widget-level active state via `id.request_active()`. Controls cursor blinking, text selection. Independent from app-level focus.

### KeyPressFocus Trait

Every keyboard-handling component implements:

- `check_condition(Condition)` — Reports which conditions are true (e.g., `ListFocus`, `EditorFocus`, `ModalFocus`).
- `run_command(command, count, mods)` — Handles matched commands. Returns `CommandExecuted::Yes` to consume.
- `receive_char(c)` — Handles typed characters that don't match any keybinding.
- `focus_only()` — Return `true` for modals to prevent background key handling.

### Keyboard Event Flow

1. Top-level view captures all `KeyDown` events
2. `WindowData::key_down()` → active workspace's `WorkspaceData::key_down()`
3. Focus-based routing: checks `Focus` enum, dispatches to appropriate `KeyPressFocus` implementor
4. Keybinding resolution: matches key event against bindings, checking `when` conditions
5. Command execution through `run_workbench_command()` or `run_internal_command()`

### Preview Editor Focus (the `preview_focused` pattern)

When a component has both a text input and a preview editor:
1. `check_condition`: When `preview_focused`, report `EditorFocus` (not `ListFocus`).
2. `run_command`/`receive_char`: Forward to preview editor when `preview_focused`.
3. Reset `preview_focused = false` on input click, list navigation.

---

## Keypress Matching

The keypress system (`keypress/`) resolves key events to commands:

### Key Event Flow

1. Event → `KeyPress` via normalization (lowercase ASCII, handle numpad, filter modifier repeats)
2. Pending buffer updated (1-second timeout for chord expiry)
3. `match_keymap()` lookup:
   - `Full(cmd)` — exact match, execute
   - `Multiple(cmds)` — multiple matches, try in reverse order (later bindings win)
   - `Prefix` — partial chord, wait for more keys
   - `None` — try without Shift for selection extension, then fall through to character input

### Multi-Key Chords

The keymaps `IndexMap` stores every prefix of every registered sequence. For "Ctrl+K Ctrl+S", entries exist for both `[Ctrl+K]` and `[Ctrl+K, Ctrl+S]`.

### Condition Expressions

`when` clauses in keybinding TOML (e.g., `when = "editor_focus && !modal_focus"`). Parser supports AND/OR with left-to-right evaluation. Unknown conditions evaluate to false (positive) or true (negated).

### Unbinding

A command prefixed with `-` removes a previously loaded binding with the same key+when combination.

### Keybinding Loading Order

1. `defaults/keymaps-common.toml` (all platforms)
2. `defaults/keymaps-macos.toml` or `defaults/keymaps-nonmacos.toml`
3. User keymaps file (can override/unbind defaults)

---

## Configuration System

### Layered Override Strategy

The config system uses the `config` crate for layered merging (lowest to highest priority):

1. **Embedded defaults** (`defaults/settings.toml`) — compiled into binary
2. **Default dark theme base** — color palette foundation
3. **Active color theme** — from plugins, local files, or embedded themes
4. **Active icon theme** — from plugins or embedded "Lapce Codicons"
5. **User global settings** (`~/Library/Application Support/dev.lapce.*/settings.toml`)
6. **Workspace-local settings** (`.lapce/settings.toml`)

### Config Structs

- `CoreConfig` (`config/core.rs`): Color theme, icon theme, titlebar mode
- `EditorConfig` (`config/editor.rs`): Font, wrapping, completion, error lens, inlay hints
- `UIConfig` (`config/ui.rs`): Scale, font sizes, header heights, status bar

### Config Reload

- `LapceConfig::load()`: Full load from all layers
- `resolve_theme()`: Re-merge with new theme, re-resolve colors
- `update_file()` / `reset_setting()`: Surgical TOML edits preserving formatting
- Config ID (timestamp) used as change marker

### File Watching

`ConfigWatcher` uses `notify` with `AtomicBool` debouncing — first event in a burst triggers a 500ms delay, subsequent events dropped.

---

## Theme System

### Color Themes

Three-tier resolution (`config/color_theme.rs`):

1. **Base colors**: Named variables like `"red" = "#E06C75"`. Support `$variable` references with recursive resolution (max depth 6).
2. **UI colors**: Semantic names like `"editor.background"` → hex values or `$variable` references.
3. **Syntax colors**: Same mechanism for syntax tokens.

`ThemeColorPreference` (Light/Dark/HighContrastLight/HighContrastDark) determined heuristically by comparing foreground/background luminance.

### Icon Themes

Resolution chain for file icons: exact filename match → extension match. Three embedded SVG directories:
- `icons/codicons/` — VS Code codicons for UI elements (monochrome)
- `icons/lapce/` — Lapce logo
- `icons/filetypes/` — Colored file-type icons

`SvgStore` provides two-tier caching: embedded SVGs (by name) and on-disk plugin SVGs (by path).

---

## Panel System

### Structure

Three physical containers (Left, Bottom, Right) each split into two halves. Panel kinds (`PanelKind` enum): `FileExplorer`, `Search`.

**Fixed layout**: Order from `default_panel_order()`. No drag-and-drop reordering.

### PanelBuilder

Builder pattern for assembling panels from foldable sections. Each section has a clickable header with fold chevron. Sections in side panels flex-grow when open; bottom panels lay out horizontally.

### Panel Container View

Assembles: two panel pickers (icon strips), two panel content views (Floem `tab` widget), and a 4px resize drag handle.

### Maximization

Only supported for the bottom container, applies to both BottomLeft and BottomRight simultaneously.

---

## Search System

### Shared Backend

`GlobalSearchData` is the shared search backend used by both the search modal and panel. Results stored in `IndexMap<PathBuf, SearchMatchData>` with per-file `expanded` state.

### Search Modal

Floating popup with flat results and preview editor. Syncs input to `GlobalSearchData.set_pattern()`. Auto-closes on focus change. Pre-populates with word at cursor. "Open in search panel" button transfers context.

### Search Panel

Bottom panel with hierarchical results (file groups → matches). 50/50 horizontal split: results + preview editor. Keyboard navigation via `visible_matches()` flattened list.

---

## Plugin System

### Architecture (Three Layers)

**Layer 1 — Plugin Catalog** (`plugin/catalog.rs`): Owns the registry of all plugins. Runs on a dedicated thread. Handles lazy activation (language, workspace_contains conditions).

**Layer 2 — Plugin Server Protocol (PSP)** (`plugin/psp.rs`): Per-plugin RPC handler managing bidirectional JSON-RPC. Handles capability checking, document selector matching, and host-side requests.

**Layer 3 — Plugin Implementations**:
- **LSP** (`plugin/lsp.rs`): Spawns language server process. Three threads: stdin writer, stdout reader, stderr logger. Content-Length framed JSON-RPC over stdio.
- **WASI** (`plugin/wasi.rs`): Compiles WASM modules via wasmtime. In-memory pipes. HTTP capability for network access. Can spawn child LSP servers.

### Broadcasting

When a request is made, the catalog sends to ALL active plugins. "First success wins" — the first successful result triggers the callback; subsequent ones are ignored.

### Plugin Management UI

`PluginData` (`plugin.rs`) manages installed (from filesystem) and available (from `plugins.lapce.dev` API) plugins. Supports install, enable/disable (global and per-workspace), update detection, and icon caching.

### Bundled Plugins

Plugins in `defaults/plugins/` are embedded at compile time via `include_dir!`. Extracted to user's plugins directory on first launch.

---

## Language Support and Syntax Highlighting

### Language Detection

`LapceLanguage` enum (~100 variants) maps to `SyntaxProperties` via array indexing:
- `TreeSitterProperties`: Grammar name, query files, code glance configuration
- `CommentProperties`: Single-line and multi-line comment tokens
- File extensions and names for detection

### Syntax Pipeline

```
Syntax
├── SyntaxLayers (HopSlotMap<LayerId, LanguageLayer>)
│   ├── root LanguageLayer (covers entire file)
│   └── child LanguageLayers (injections, e.g. JS in HTML)
│         ├── tree: Option<Tree> (tree-sitter parse tree)
│         └── config: Arc<HighlightConfiguration>
├── styles: Option<Spans<Style>> (highlight spans)
├── normal_lines: Vec<usize> (code glance visible lines)
└── BracketParser (rainbow bracket colorization)
```

### Key Design Decisions

- **Thread-local parser caching**: Tree-sitter parsers/cursors in thread-local storage for reuse without cross-thread sharing.
- **Library leaking**: `std::mem::forget(library)` after loading grammar .so files — function pointers must remain valid.
- **Dual bracket parsers**: Tree-sitter-aware `walk_tree_bracket_ast` for supported languages, fallback naive text-based `BracketParser` for others.
- **Injection reuse via hashing**: Existing injection layers identified by hashing language + ranges + depth to avoid re-parsing.

---

## Persistence Layer

`LapceDb` (`db.rs`) saves state as JSON files in the config directory:

- **App state** (`db/app`): Window positions/sizes
- **Window layout** (`db/window`): For single-window restore
- **Workspace layout** (`db/workspaces/<name>/workspace_info`): Full split tree + panel config
- **Document positions** (`db/workspaces/<name>/workspace_files/<sha256>`): Per-file cursor/scroll
- **Disabled plugins** (`db/disabled_volts`)
- **Recent workspaces** (`db/recent_workspaces`)

**Async saves**: All writes go through a `crossbeam_channel` to a dedicated thread. Reads are synchronous (startup only).

---

## RPC Protocol

### Message Types

**UI → Proxy:**
- `ProxyRequest`: Request-response pairs (get hover, search, LSP operations)
- `ProxyNotification`: Fire-and-forget (text edits, completion triggers, plugin management)

**Proxy → UI:**
- `CoreNotification`: Almost entirely notification-based (completions, diagnostics, file changes, progress)

### Request Correlation

`ProxyRpcHandler` auto-increments `RequestId`, stores callbacks in `pending` map. On response, `handle_response()` invokes the matching callback.

### Transport Format

Single-line JSON with newline delimiters. Serde tag format: `{"method": "...", "params": {...}}` with added `"id"` field for requests.

---

## Concurrency Model

### Thread Taxonomy

1. **UI thread** (single): Floem event loop, rendering, reactive signal processing
2. **Dispatcher thread** (single): Owns all mutable proxy state. Sequential processing prevents data races.
3. **Catalog thread** (single): Owns plugin registry. All plugin lifecycle serialized.
4. **Per-plugin threads**: LSP gets 3 (reader, writer, stderr). WASI gets 2 (I/O, mainloop). Plus `PluginServerRpcHandler.mainloop` per plugin.
5. **Work threads** (transient): Global search, file listing, plugin install, grammar download.
6. **File watcher thread**: `notify` event processing loop.
7. **DB save thread**: Processes async write operations.

### Concurrency Primitives

- `crossbeam_channel::unbounded()` — Primary inter-thread communication
- `parking_lot::Mutex` — Shared mutable state (pending responses, watcher state)
- `Arc<AtomicU64>` / `Arc<AtomicUsize>` — Lock-free coordination (search cancellation, request counting)
- Floem reactive signals — UI state management (single-threaded access)
