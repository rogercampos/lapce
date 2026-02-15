# Lapce Developer Guide

**Important:** Always run `cargo fmt --all` after finishing any code changes to ensure consistent formatting.

**Important:** Do not care about backwards compatibility of persisted data in a particular installation of this software. We're actively developing this, nobody is using it yet.

## Build & Run

```bash
# Development build (faster compile, no optimizations)
cargo build --profile fastdev
cargo run --profile fastdev --bin lapce

# Release build
cargo build --release

# Run all CI checks locally (fmt, clippy, build, doc tests)
make ci
```

Rust toolchain must be installed via rustup. On macOS, the stable toolchain is at `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo`.

## Testing & Coverage

```bash
# Run ALL tests (unit + integration)
cargo test --workspace

# Run only doc tests (this is what `make test` and `make ci` run)
cargo test --doc --workspace

# Code coverage — terminal summary
make coverage-summary

# Code coverage — HTML report at target/llvm-cov/html/index.html
make coverage
```

Coverage uses `cargo-llvm-cov` (install: `cargo install cargo-llvm-cov`). Note: `make test` only runs doc tests, not the full unit test suite.

## Crate Structure

```
lapce/
├── lapce-app/     UI application (Floem framework, all views and state)
├── lapce-proxy/   Separate process for LSP, file I/O
├── lapce-rpc/     RPC message types shared between app and proxy
└── lapce-core/    Re-exports floem_editor_core (rope, commands, cursor, syntax)
```

**Dependency flow:** `lapce-app` → `lapce-proxy` → `lapce-rpc` → `lapce-core` → `floem_editor_core`

## Two-Process Architecture

The app runs two processes communicating via channels:

```
┌─────────────┐  ProxyRequest/Notification  ┌─────────────┐
│  lapce-app  │ ──────────────────────────→ │ lapce-proxy  │
│   (UI)      │ ←────────────────────────── │  (backend)   │
└─────────────┘     CoreNotification        └─────────────┘
```

- **App process**: UI rendering, editor state, user input (Floem reactive framework)
- **Proxy process**: LSP clients, file watching, global search

Messages defined in `lapce-rpc/src/proxy.rs` (ProxyRequest, ProxyNotification, ProxyResponse) and `lapce-rpc/src/core.rs` (CoreNotification). Communication uses `crossbeam_channel::unbounded()`.

Proxy is spawned in `lapce-app/src/proxy.rs` → `new_proxy()`. The dispatcher lives in `lapce-proxy/src/dispatch.rs`. The dispatcher processes messages sequentially to prevent data races on buffer state.

**Reactive bridge:** `CoreNotification` messages from the proxy are bridged to Floem's reactive system via `create_signal_from_channel()`, which returns a `ReadSignal` that an effect subscribes to for processing.

## App State Hierarchy

```
AppData
└── WindowData (one per OS window)
    └── WindowTabData (one per workspace tab)
        ├── MainSplitData (recursive editor splits)
        │   ├── SplitData { children, direction }
        │   └── EditorTabData → EditorData → Doc
        ├── PanelData (file explorer, search, problems, etc.)
        ├── PaletteData (command palette)
        └── CommonData (config, proxy handle, diagnostics, shared signals)
```

All state uses **Floem reactive signals** (`RwSignal<T>`, `ReadSignal<T>`, `Memo<T>`). UI re-renders automatically when signals change. Key pattern: `signal.get()` (tracked, triggers re-render) vs `signal.get_untracked()` (no subscription).

**Immutable data structures:** Split tree and editor state use the `im` crate's persistent data structures (`im::HashMap`, `im::Vector`) for efficient structural sharing — updating one entry creates a new map that shares most of its structure with the old one. This works well with the reactive signal system.

## Key Source Files (lapce-app/src/)

| File | Purpose |
|------|---------|
| `app.rs` | App entry, window creation, `workbench()` layout function |
| `window.rs` | Window state, multi-tab management |
| `window_tab.rs` | Per-workspace state, wires everything together |
| `main_split.rs` | Recursive editor split tree |
| `editor.rs` | Editor logic: commands, cursor, completion, hover |
| `editor_tab.rs` | Tab container for editors |
| `doc.rs` | Document/buffer: rope text, syntax, diagnostics, find |
| `command.rs` | All commands: `LapceWorkbenchCommand`, `InternalCommand`, `CommandKind` |
| `config.rs` + `config/` | Config loading: `CoreConfig`, `EditorConfig`, `UIConfig` |
| `db.rs` | Persistence: workspace state, recent files |
| `proxy.rs` | Proxy process spawning and RPC bridge |
| `panel/` | Panel system: `kind.rs` (types), `data.rs` (state), `view.rs` (rendering) |
| `palette/` | Command palette: `kind.rs` (modes with prefix symbols like `/`, `@`, `:`) |
| `recent_files.rs` | Recent files popup: data, KeyPressFocus impl, view (uses `exclusive_popup`) |
| `search_modal.rs` | Search modal popup: text input + flat results + preview, syncs with GlobalSearchData |
| `global_search.rs` | Global search data: hierarchical results, preview state, keyboard navigation |
| `panel/global_search_view.rs` | Global search panel view: horizontal split (results + preview editor) |
| `file_icon.rs` | Reusable file icon + filename view helpers (`file_icon_svg`, `file_icon_with_name`) |
| `keypress/` | Keybinding resolution and condition evaluation |

## Initialization Flow

Startup sequence from `main()`:

1. **CLI parsing** (Clap): `--new`, `--wait`, positional path args
2. **Panic hook + logging**: Custom panic handler. Dual-output tracing: daily-rotated file logger + console
3. **Shell environment capture**: When launched from GUI (not terminal), spawns a login shell to capture `printenv` for PATH
4. **Process re-spawn**: Without `--wait`, re-spawns with `--wait` and exits immediately to unblock terminal
5. **Single-instance check**: Tries to connect to existing Lapce instance via local socket. If found, sends paths to open and exits
6. **Core init**: DB, file watchers, config load
7. **Window creation**: Restores from DB or creates new windows from CLI args
8. **Background threads**: Config watcher, grammar updater (checks GitHub for tree-sitter releases), update checker, local socket listener
9. **Event loop**: `app.run()` enters Floem event loop

## Editor System

### EditorData

`EditorData` (`editor.rs`) wraps floem's `Editor` with Lapce-specific state: snippet tracking (tab-stops updated via `Transformer` on buffer edits), find state, editor kind (`Normal` vs `Preview`), and shared `CommonData`.

**Clone semantics**: `EditorData` is cheaply cloneable (all fields are signals or `Rc`). The `copy()` method creates a new editor sharing the same `Doc` but with independent cursor/viewport.

### Editor Rendering Pipeline

`EditorView::paint()` renders layers in order: current line highlight → selection rectangles → find result outlines → bracket highlights + scope lines → text + phantom text → sticky headers → scroll bar.

**Sticky headers**: Computed in an effect watching viewport, buffer rev, and screen lines. Finds enclosing syntax scopes and animates push-up at scope boundaries.

## Document Model

`Doc` (`doc.rs`) — one per file, shared via `Rc` across all editors viewing the same file:

- **Buffer** (`RwSignal<Buffer>`): Rope-based text buffer from xi-rope
- **Dual styling**: Semantic styles (LSP semantic tokens) take priority over tree-sitter syntax styles. Both stored as `Spans<Style>` and shifted on edits
- **Phantom text**: Virtual text assembled per-line — inlay hints, error lens, completion lens, inline completion, preedit (IME)
- **`cache_rev` signal**: Incremented when visual representation changes even without text changes (e.g., new inlay hints). Use this to invalidate visual caches
- **Background processing**: Syntax parsing runs on rayon threads with cancellation via `AtomicUsize`. Find operations also use rayon
- **Three constructors**: `new()` (from file), `new_content()` (scratch/local), `new_history()` (for diff view)

## Split Tree Architecture

The editor area (`MainSplitData` in `main_split.rs`) uses a recursive tree with key design decisions:

- **Flat storage**: All nodes in flat `im::HashMap` maps (`splits`, `editor_tabs`, `editors`) keyed by ID. Tree structure encoded via parent/child ID fields — enables O(1) lookup
- **Geometry-based focus navigation**: `split_move()` uses physical screen coordinates (layout_rect + window_origin) to find adjacent panes, rather than tree traversal. Works regardless of nesting depth
- **Tree collapse**: When a split has only one child left, the child is promoted to replace the split, preventing unnecessary nesting
- **Document sharing**: The `docs` map caches `Rc<Doc>` by path. Multiple editors share the same `Doc`
- **Dual navigation history**: Global (`MainSplitData.locations`) for cross-split jumps; per-tab (`EditorTabData.locations`) for local back/forward
- **Tab reuse policy**: `get_editor_tab_child()` implements tab reuse — with `show_tab` disabled, reuses any pristine editor or one showing the same path; with `show_tab` enabled, searches all editor tabs for existing tabs

## Command System

Commands are defined as strum enums in `command.rs`:

- `LapceWorkbenchCommand` — user-facing (appear in palette), decorated with `#[strum(message = "...")]`
- `InternalCommand` — app-internal (not in palette), sent via `common.internal_command.send()`
- `CommandKind` — wraps all command types (Workbench, Edit, Move, Scroll, Focus, MotionMode, MultiSelection)

Edit/Move/Scroll/Focus/MotionMode/MultiSelection commands come from `floem_editor_core::command` (external dep), re-exported through `lapce-core`.

`lapce_internal_commands()` builds the registry of commands available in the palette. Commands not registered there won't appear.

### Command Dispatch

`WorkspaceData` uses three listener channels:
- `lapce_command`: Delegates to workbench_command or active editor
- `workbench_command`: UI-level actions (palette, panels, file operations)
- `internal_command`: Implementation-detail actions (~40 variants with rich data payloads)

## LSP Module (lapce-proxy/src/lsp/)

Direct LSP integration without a plugin runtime. Language servers are defined as entries in a built-in Rust `const` array (`LSP_SERVERS` in `manager.rs`).

- `mod.rs` — `LspRpcHandler`: crossbeam channel-based RPC interface (same method signatures as old plugin catalog handler). Also contains `client_capabilities()`.
- `manager.rs` — `LspManager`: manages multiple LSP server instances. Language-based routing (one server per language). Lazy activation on first `didOpen` for a matching language. Tracks open files for replay on late server start.
- `client.rs` — `LspClient`: spawns LSP subprocess, 3 I/O threads (stdin writer, stdout reader, stderr logger), Content-Length framed JSON-RPC, request/response correlation, server capability tracking.

**Adding a new language server** = adding one `LspServerConfig` entry to the `LSP_SERVERS` array. No other code changes needed. Server commands resolved via PATH using the captured shell environment (supporting mise/asdf/rbenv/rvm).

`PluginId` is kept as an internal type for LSP server instance routing (used in completion/code action resolve).

## Floating Popups / Modal System

Floating modals (popups that appear centered over the editor with a dimmed backdrop) use the reusable `exclusive_popup()` function in `about.rs`:

```rust
pub fn exclusive_popup(config, visibility, on_close, content) -> impl View
```

It provides: dimmed overlay (`Position::Absolute`, full-screen), click-outside-to-close (outer container captures `PointerDown`), inner content prevents propagation, centered with flex. The content is responsible for its own styling (padding, border, background, border-radius) — `exclusive_popup` only handles the overlay and centering.

### Adding a new floating popup

1. **Create data struct** in a new module (e.g., `recent_files.rs`). Hold `visible: RwSignal<bool>` and any state. Implement `KeyPressFocus` trait for keyboard handling (ESC via `FocusCommand::ModalClose`, list navigation via `ListNext`/`ListPrevious`/`ListSelect`).

2. **Add `Focus::YourPopup`** variant to the `Focus` enum in `window_tab.rs`.

3. **Add a `LapceWorkbenchCommand`** variant in `command.rs` with `#[strum(serialize = "...")]` and `#[strum(message = "...")]`.

4. **Wire in `WindowTabData`** (`window_tab.rs`):
   - Add the data field to the struct and initialize in `new()`.
   - Add `Focus::YourPopup => Some(keypress.key_down(event, &self.your_data))` in `key_down()`.
   - Add the command handler in `run_workbench_command()`.

5. **Create the view** using `exclusive_popup()` from `about.rs`, and add it to the floating layers stack in `window_tab()` in `app.rs`. Order in the stack = z-order (later items render on top).

6. **Add keybinding** in `defaults/keymaps-{macos,nonmacos,common}.toml`.

Existing popups using this pattern: About dialog (`about.rs`), Recent Files (`recent_files.rs`), Search Modal (`search_modal.rs`). The alert dialog (`alert.rs`) uses a similar but separate pattern.

### Text input in popups

Use `TextInputBuilder::new().is_focused(is_focused_fn).build_editor(editor_data)` (from `text_input.rs`). Create the `EditorData` via `main_split.editors.make_local(cx, common)`. The `is_focused` function should check `focus.get() == Focus::YourPopup`.

### Fuzzy filtering

The `nucleo` crate is available as a dependency. For small lists (< ~1000 items), use it directly in a `Memo`. For large lists (like the file palette), the palette uses a separate thread — see `palette.rs` for that pattern.

## Layout Structure (app.rs → workbench())

```
Vertical stack (flex_col) {
    Horizontal stack {
        Left panel container    // File Explorer
        Editor area (main_split)
        Right panel container   // (empty by default)
    }
    Bottom panel container      // Search, Problems, Call Hierarchy, etc.
}
```

Panel containers hide automatically when empty. Layout defined in `workbench()` in `app.rs`.

**Floating overlay z-order** (workspace view layers on top of workbench):
```
completion → hover → code_action → rename → palette
→ search_modal → recent_files → about → alert
```

If no folder is open, shows `empty_workspace_view()` — a centered "Open Folder" button.

## Panel System

Panels defined in `panel/kind.rs` as `PanelKind` enum. Each has a default position (`PanelPosition`: LeftTop, LeftBottom, BottomLeft, BottomRight, RightTop, RightBottom).

**Panel layout is fixed** — the order always comes from `default_panel_order()` in `panel/data.rs`. There is no drag-and-drop reordering. Default visibility (shown/hidden) set in `PanelData::new()` via `PanelStyle`.

**Persistence caveat:** Panel styles (active tab, shown, maximized), sizes, and section fold states are persisted per-workspace in `db/workspaces/<hash>/workspace_info`. Changes to style defaults only take effect for new workspaces or after deleting persisted state.

## Configuration

Config structs in `config/`: `core.rs`, `editor.rs`, `ui.rs`. Defaults in `defaults/settings.toml`.

Keybindings in `defaults/keymaps-{common,macos,nonmacos}.toml`. Each binding has: `key`, `command`, optional `mode` (i=insert, n=normal, v=visual), optional `when` (condition).

### Layered Override Strategy (lowest to highest priority)

1. **Embedded defaults** (`defaults/settings.toml`) — compiled into binary
2. **Default dark theme base** — color palette foundation
3. **Active color theme** — from local files or embedded themes
4. **Active icon theme** — embedded "Lapce Codicons"
5. **User global settings** (`~/Library/Application Support/dev.lapce.*/settings.toml`)
6. **Workspace-local settings** (`.lapce/settings.toml` in workspace root)

The app name includes the build type — debug builds use `Lapce-Debug`, release uses `Lapce`. User config dir: `Directory::config_directory()`.

**Config reload**: `LapceConfig::load()` for full reload, `resolve_theme()` for theme-only re-merge, `update_file()`/`reset_setting()` for surgical TOML edits preserving formatting. Config ID (timestamp) used as change marker.

**File watching**: `ConfigWatcher` uses `notify` with `AtomicBool` debouncing — first event triggers a 500ms delay, subsequent events in the burst are dropped.

## Persistence & Data Directory

`LapceDb` in `db.rs` saves state asynchronously via a dedicated thread. Storage at `config_dir/db/`:

- `app` — window positions/sizes
- `window` — window layout (for single-window restore)
- `workspaces/<name>/workspace_info` — per-workspace: full split tree + panel config
- `workspaces/<name>/workspace_files/<sha256>` — per-file cursor/scroll position
- `recent_workspaces` — recent workspace list

**Important:** When changing panel style defaults, you must delete `db/workspaces/` to see changes, because the app loads persisted state over defaults.

## Focus System & Keyboard Routing

The app has a two-level focus system:

1. **App-level focus** (`Focus` enum in `window_tab.rs`): Determines which component receives keyboard events. Set via `common.focus.set(Focus::Variant)`. The `key_down()` method in `WindowTabData` dispatches to the appropriate `KeyPressFocus` implementor based on the current `Focus` value.

2. **Floem-level focus**: Widget-level active state managed by `id.request_active()`. This controls cursor blinking, text selection, etc. Independent from app-level focus.

### KeyPressFocus trait

Every component that handles keyboard input implements `KeyPressFocus`:

- `check_condition(Condition)` — Reports which conditions are true for keybinding matching. Key conditions: `ListFocus` (enables up/down/enter for list navigation), `EditorFocus` (enables editor keybindings), `ModalFocus` (enables ESC to close), `PanelFocus`.
- `run_command(command, count, mods)` — Handles matched commands. Return `CommandExecuted::Yes` to consume.
- `receive_char(c)` — Handles typed characters that don't match any keybinding.
- `focus_only()` — Return `true` for modals to prevent background key handling.

### Keypress Matching Pipeline

1. Key event → `KeyPress` via normalization (lowercase ASCII, handle numpad, filter modifier-only repeats)
2. Pending buffer updated (1-second timeout for chord expiry)
3. `match_keymap()` lookup yields:
   - `Full(cmd)` — exact match, execute
   - `Multiple(cmds)` — multiple matches, try in reverse order (later bindings win)
   - `Prefix` — partial chord, wait for more keys
   - `None` — try without Shift for selection extension, then fall through to `receive_char()`

**Multi-key chords**: The keymaps `IndexMap` stores every prefix of every registered sequence. For "Ctrl+K Ctrl+S", entries exist for both `[Ctrl+K]` and `[Ctrl+K, Ctrl+S]`.

**Keybinding loading order**: (1) `defaults/keymaps-common.toml`, (2) `defaults/keymaps-macos.toml` or `defaults/keymaps-nonmacos.toml`, (3) user keymaps file. Later bindings can override or unbind (prefix command with `-`) earlier ones.

**Condition expressions**: `when` clauses support AND (`&&`) / OR (`||`) with left-to-right evaluation. Unknown conditions evaluate to false (positive) or true (negated).

### Keybinding conditions

Keybindings in TOML files have `when` clauses (e.g., `when = "list_focus"`). The condition is checked against the focused component's `check_condition()`. If a component doesn't report `ListFocus`, then `list.next`/`list.previous`/`list.select` bindings (bound to up/down/enter) won't fire for that component.

### EditorData::pointer_down() and Focus

**Critical gotcha:** `EditorData::pointer_down()` (`editor.rs`) forcefully sets `common.focus.set(Focus::Workbench)` when the editor's document is non-local (a file). This is correct for main workbench editors but **breaks preview editors** in modals and panels by stealing focus. The guard `self.kind.get_untracked().is_normal()` ensures only normal editors (not `EditorViewKind::Preview`) change focus.

### Preview Editors

Preview editors are created with `main_split.editors.make_local(cx, common)` and have `editor_tab_id = None`. They are used in the search modal, global search panel, and palette.

Key properties:
- `EditorViewKind::Preview` — disables sticky headers (via `is_normal()` checks in `view.rs`), prevents focus stealing in `pointer_down()`
- No `editor_tab_id` — `FocusEditorTab` internal command is not sent on click
- Keyboard events must be routed through the parent component's `KeyPressFocus` implementation

### Making preview editors editable (the `preview_focused` pattern)

When a component has both a text input and a preview editor, use a `preview_focused: RwSignal<bool>` signal to track which sub-component should receive keyboard input:

1. **`check_condition`**: When `preview_focused`, report `EditorFocus` (not `ListFocus`) so editor keybindings work and list navigation doesn't intercept arrows.
2. **`run_command`**: When `preview_focused`, forward commands to `preview_editor.run_command()`.
3. **`receive_char`**: When `preview_focused`, forward to `preview_editor.receive_char()`.
4. **View**: Add `on_event_cont(EventListener::PointerDown)` on the preview container to set `preview_focused = true`.
5. **Reset**: Set `preview_focused = false` when clicking results, clicking the input, or using list navigation (next/previous).

### Floem Event Propagation

- `on_event_cont` — Handler fires, event continues bubbling (propagation NOT stopped)
- `on_event_stop` — Handler fires, event propagation STOPPED
- `on_click_stop` — Convenience for PointerUp with stop
- Events bubble from child to parent. The editor content view uses `on_event_cont` for PointerDown, so clicks bubble to parent containers.
- In `exclusive_popup`, the content wrapper uses `on_event_stop(PointerDown)` to prevent clicks from reaching the outer close handler.

## Search System

### Search Modal (`search_modal.rs`)

A floating popup (`exclusive_popup`) with text input, flat results list, and preview editor. Uses `Focus::SearchModal`. The input editor syncs its text to `GlobalSearchData::set_pattern()`, sharing the search backend. Results are a `Memo<Vec<FlatSearchMatch>>` derived from `GlobalSearchData::search_result`.

### Global Search Panel (`global_search.rs` + `panel/global_search_view.rs`)

A bottom panel (`PanelKind::Search`) with hierarchical results grouped by file (`IndexMap<PathBuf, SearchMatchData>`). Each file group has `expanded: RwSignal<bool>`. The panel shows a 50/50 horizontal split: results on the left, preview editor on the right. Uses `Focus::Panel(PanelKind::Search)`.

Navigation through hierarchical results requires building a flat list of visible matches from expanded files (the `visible_matches()` helper).

## Quirks & Gotchas

- **floem_editor_core re-export:** `lapce-core` does `pub use floem_editor_core::*`. Types like `MultiSelectionCommand`, `EditCommand`, `MoveCommand` originate in floem but are imported via `lapce_core::command`. You cannot import them directly from floem (they're private there).

- **Mode::Terminal exists but is unused:** Defined in floem's `Mode` enum (external dep). Cannot be removed without forking floem. Just ignore it.

- **Build profiles matter:** `cargo build` (dev) and `cargo build --profile fastdev` produce separate artifacts. If you build with one profile but run with another, you get stale code.

- **Debug vs Release app name:** The app name (`Lapce-Debug` vs `Lapce`) is generated at build time in `lapce-core/build.rs` → `meta.rs`. This affects the config/data directory path.

- **Proxy runs in-process for local:** Despite the two-process architecture, local workspaces run the proxy as a thread within the same process (see `proxy.rs`). The separate-process mode was for remote development (now removed).

- **Signal tracking:** Using `signal.get()` inside a view subscribes to updates. Using it in command handlers or initialization should use `get_untracked()` to avoid unnecessary re-renders. Misusing tracked gets in non-view code can cause performance issues. **Especially in `create_effect`:** using `signal.get()` inside an effect makes the effect re-run whenever that signal changes. If the effect triggers a side-effect (network request, thread spawn, etc.), use `get_untracked()` for signals that should only be read once rather than tracked.

- **`virtual_stack` performance:** The item view builder closure (third argument to `virtual_stack`) runs once per visible item. Expensive computations that depend on the full list should be precomputed as a `Memo` or `create_memo` outside the `virtual_stack`, not re-computed per-item inside the builder closure.

- **Panel toggle commands:** `toggle_*_focus` shows the panel AND focuses it. `toggle_*_visual` only toggles visibility. The keyboard shortcuts use the `_focus` variants.

- **Strum enum serialization:** Command names in keybindings must match the `#[strum(serialize = "...")]` attribute exactly. The command palette uses `get_message()` for display names.

- **Adding new UI icons:** Requires two steps: (1) add a constant to `lapce-app/src/config/icon.rs` (e.g. `pub const FOO: &'static str = "foo";`), (2) map it in `defaults/icon-theme.toml` under `[icon-theme.ui]` (e.g. `"foo" = "some-codicon.svg"`). Available SVGs are in `icons/codicons/` (~158 files) and `icons/lapce/`.

- **Adding new file type icons:** Add colored SVG to `icons/filetypes/`, map the extension in `defaults/icon-theme.toml` under `[icon-theme.extension]` (e.g. `"rb" = "ruby.svg"`). For filename matches (e.g. `Dockerfile`), use `[icon-theme.filename]`. The SVGs are embedded at compile time via `FILETYPES_ICONS_DIR` in `config/svg.rs`. The fallback chain in `files_svg()` is: default theme embedded filetypes → generic file.svg.

- **PanelBuilder custom headers:** `PanelBuilder` in `panel/view.rs` has `add()` (string header) and `add_with_header()` (custom View header). Both delegate to `add_general_with_header()` → `foldable_panel_section()`. Buttons inside the header using `clickable_icon()` won't trigger fold because `on_click_stop` stops propagation.

- **Active editor file path:** `window_tab_data.main_split.active_editor` is a `Memo<Option<EditorData>>`. Get file path: `editor_data.doc().content.get_untracked()` then match `DocContent::File { path, .. }`. Use `get_untracked()` in handlers, not view code.

- **File explorer reveal:** `FileExplorerData::reveal_in_file_tree(path)` in `file_explorer/data.rs` opens ancestor dirs, reads unread dirs async, scrolls to file, and selects it. The `RevealInPanel` workbench command wraps this with panel show/open logic.

- **Keybinding conflicts with `when` conditions:** When adding a new keybinding, check for conflicts with existing bindings on the same key. A binding with a broader `when` condition (e.g., `!source_control_focus`) will match before a more specific one (e.g., `modal_focus`) if both conditions are true. The first matching binding wins. Use narrow conditions to avoid conflicts (e.g., `editor_focus && !modal_focus`).

- **Native menu items have no keyboard accelerators:** Floem's `MenuItem` (`floem-local/src/menu.rs`) doesn't support accelerators — the `accelerator` parameter is hardcoded to `None` when calling `muda::MenuItem::with_id()`. This means **all keyboard shortcuts must be defined in the keymaps TOML files**, even standard OS shortcuts like Cmd+Q. Adding a menu item in `app.rs` does NOT give it a keyboard shortcut; you must also add a keybinding in `defaults/keymaps-{macos,nonmacos,common}.toml` that triggers the same command.

- **`make_local` editors have no `editor_tab_id`:** Editors created via `main_split.editors.make_local()` get `editor_tab_id = None`. This means `pointer_down()` won't send `FocusEditorTab`, and many focus commands that require `editor_tab_id` (split, close tab, etc.) will return `CommandExecuted::No`. This is by design for preview/local editors.

- **Floem gradient backgrounds:** Floem supports gradient backgrounds via peniko's `Gradient` type passed to `.background()`. **Only `Gradient::new_linear()` works reliably.** `Gradient::new_radial()` and `new_two_point_radial()` render as all-white when used with `.background()` in the style system — the Vello renderer doesn't handle them correctly in this context. Workaround: use angled linear gradients to approximate radial effects. Import: `use floem::peniko::{Gradient, kurbo::Point};`. Example: `Gradient::new_linear(Point::new(0.0, 0.0), Point::new(800.0, 400.0)).with_stops([(0.0, color_a), (1.0, color_b)])`.

- **Adding new theme colors:** Requires changes in 3 places: (1) add a `pub const` in `lapce-app/src/config/color.rs` (`LapceColor` impl), (2) add a base variable + UI mapping in `defaults/dark-theme.toml` (`[color-theme.base]` + `[color-theme.ui]`), (3) same in `defaults/light-theme.toml`. Access via `config.color(LapceColor::YOUR_CONSTANT)` which returns `floem::peniko::Color`.

## Language Support & Syntax Highlighting

`LapceLanguage` enum (~65 variants in `lapce-core/src/language.rs`) maps to `SyntaxProperties` via array indexing. Each entry contains `TreeSitterProperties` (grammar name, query files, code glance config), `CommentProperties` (single/multi-line tokens), and file extensions/names for detection.

**Syntax pipeline**: `Syntax` holds `SyntaxLayers` (a `HopSlotMap` of `LanguageLayer`s) supporting multi-layer injection (e.g., JS inside HTML). Each layer has its own tree-sitter parse tree and highlight configuration.

Key implementation details:
- **Thread-local parser caching**: Tree-sitter parsers/cursors stored in thread-local storage for reuse without cross-thread sharing
- **Library leaking**: `std::mem::forget(library)` after loading grammar `.so` files — function pointers must remain valid for the process lifetime
- **Injection reuse via hashing**: Existing injection layers identified by hashing language + ranges + depth to avoid re-parsing
- **Dual bracket parsers**: Tree-sitter-aware `walk_tree_bracket_ast` for supported languages, fallback naive text-based `BracketParser` for others

## Concurrency Model

### Thread Taxonomy

1. **UI thread** (single): Floem event loop, rendering, reactive signal processing
2. **Dispatcher thread** (single): Owns all mutable proxy state. Sequential processing prevents data races
3. **LSP manager thread** (single): Owns `LspManager`. All LSP lifecycle operations serialized
4. **Per-LSP-server threads** (3 per server): stdin writer, stdout reader, stderr logger
5. **Work threads** (transient): Global search, file listing, grammar download, syntax parsing (rayon)
6. **File watcher thread**: `notify` event processing loop
7. **DB save thread**: Processes async write operations

### Concurrency Primitives

- `crossbeam_channel::unbounded()` — Primary inter-thread communication
- `parking_lot::Mutex` — Shared mutable state (pending responses, watcher state)
- `Arc<AtomicU64>` / `Arc<AtomicUsize>` — Lock-free coordination (search cancellation, request counting)
- Floem reactive signals — UI state management (single-threaded access only)

## Theme System

### Color Themes

Three-tier resolution (`config/color_theme.rs`):

1. **Base colors**: Named variables like `"red" = "#E06C75"`. Support `$variable` references with recursive resolution (max depth 6)
2. **UI colors**: Semantic names like `"editor.background"` → hex values or `$variable` references
3. **Syntax colors**: Same mechanism for syntax tokens

`ThemeColorPreference` (Light/Dark/HighContrastLight/HighContrastDark) determined heuristically by comparing foreground/background luminance.

## Reusable View Helpers

### File Icon Views (`file_icon.rs`)

Two helpers for rendering file-type icons consistently across the UI:

- `file_icon_svg(config, path_fn)` — renders a file type SVG icon with correct size and color from `config.file_svg()`. Returns an `impl View`.
- `file_icon_with_name(config, path_fn, name_fn, folder_fn)` — renders icon + filename label + dimmed folder hint as a horizontal stack. Used in recent files, global search results, and problem panel.

These should be used whenever showing a file entry with its icon. Call sites that are too specialized (file palette with fuzzy highlighting, editor tabs with unsaved indicators) render icons directly.

## Icon Theme System

Three embedded icon directories in `config/svg.rs`:
- `icons/codicons/` — 158 monochrome SVGs from VS Code codicons (UI icons)
- `icons/lapce/` — Lapce logo only
- `icons/filetypes/` — ~24 colored SVGs for file type differentiation (devicon, MIT licensed)

Icon resolution for files (`config.rs` → `files_svg()`):
1. Default theme's `extension`/`filename` mappings → loads SVG from embedded `FILETYPES_ICONS_DIR`
2. Falls back to generic `file.svg` with editor icon color

Icon resolution for UI elements (`config.rs` → `ui_svg()`):
1. Default theme's `ui` map → loads from embedded `CODICONS_ICONS_DIR`

When `use_editor_color` is `None` or `false` in the icon theme, colored SVGs retain their original colors. This is important for file type icons.
