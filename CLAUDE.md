# Lapce Developer Guide

## Build & Run

```bash
# Development build (faster compile, no optimizations)
cargo build --profile fastdev
cargo run --profile fastdev --bin lapce

# Release build
cargo build --release
```

Rust toolchain must be installed via rustup. On macOS, the stable toolchain is at `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo`.

## Crate Structure

```
lapce/
├── lapce-app/     UI application (Floem framework, all views and state)
├── lapce-proxy/   Separate process for LSP, plugins, file I/O
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
- **Proxy process**: LSP clients, WASI plugin runtime, file watching, global search

Messages defined in `lapce-rpc/src/proxy.rs` (ProxyRequest, ProxyNotification, ProxyResponse) and `lapce-rpc/src/core.rs` (CoreNotification). Communication uses `crossbeam_channel::unbounded()`.

Proxy is spawned in `lapce-app/src/proxy.rs` → `new_proxy()`. The dispatcher lives in `lapce-proxy/src/dispatch.rs`.

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
        ├── PluginData
        └── CommonData (config, proxy handle, diagnostics, shared signals)
```

All state uses **Floem reactive signals** (`RwSignal<T>`, `ReadSignal<T>`, `Memo<T>`). UI re-renders automatically when signals change. Key pattern: `signal.get()` (tracked, triggers re-render) vs `signal.get_untracked()` (no subscription).

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
| `db.rs` | Persistence: panel order, workspace state, recent files |
| `proxy.rs` | Proxy process spawning and RPC bridge |
| `panel/` | Panel system: `kind.rs` (types), `data.rs` (state/order), `view.rs` (rendering) |
| `palette/` | Command palette: `kind.rs` (modes with prefix symbols like `/`, `@`, `:`) |
| `keypress/` | Keybinding resolution and condition evaluation |

## Command System

Commands are defined as strum enums in `command.rs`:

- `LapceWorkbenchCommand` — user-facing (appear in palette), decorated with `#[strum(message = "...")]`
- `InternalCommand` — app-internal (not in palette), sent via `common.internal_command.send()`
- `CommandKind` — wraps all command types (Workbench, Edit, Move, Scroll, Focus, MotionMode, MultiSelection)

Edit/Move/Scroll/Focus/MotionMode/MultiSelection commands come from `floem_editor_core::command` (external dep), re-exported through `lapce-core`.

`lapce_internal_commands()` builds the registry of commands available in the palette. Commands not registered there won't appear.

## Plugin System (lapce-proxy/src/plugin/)

Plugins are called **Volts** (`VoltID` = author/name, `PluginId` = runtime instance).

- `catalog.rs` — Plugin lifecycle: install, activate, deactivate, config updates
- `psp.rs` — Plugin Server Protocol (Lapce's custom RPC for plugins)
- `lsp.rs` — LSP client: spawns language server process, stdin/stdout communication
- `wasi.rs` — WASM plugin runtime via wasmtime with WASI interface

Plugins can provide: LSP servers, syntax grammars, themes, custom commands.

### Bundled Plugins

Plugins can be bundled into the binary at compile time via the `defaults/plugins/` directory. Each subdirectory represents a plugin and should contain a `volt.toml` plus any associated files (theme TOMLs, WASM binaries, etc.).

On every app launch, `install_bundled_plugins()` (in `app.rs`) checks each bundled plugin against the user's plugins directory (`Directory::plugins_directory()`). If a plugin directory doesn't already exist at the destination, it is extracted. Existing plugins are not overwritten, preserving user customizations.

The embedding uses `include_dir!` (same mechanism as SVG icons in `config/svg.rs`), so adding a new default plugin is as simple as placing its directory under `defaults/plugins/` — no code changes needed.

## Layout Structure (app.rs → workbench())

```
Vertical stack (flex_col) {
    Horizontal stack {
        Left panel container    // File Explorer, Plugin
        Editor area (main_split)
        Right panel container   // (empty by default)
    }
    Bottom panel container      // Search, Problems, Call Hierarchy, etc.
}
```

Panel containers hide automatically when empty. Layout defined in `workbench()` in `app.rs`.

## Panel System

Panels defined in `panel/kind.rs` as `PanelKind` enum. Each has a default position (`PanelPosition`: LeftTop, LeftBottom, BottomLeft, BottomRight, RightTop, RightBottom).

Default order set in `panel/data.rs` → `default_panel_order()`. Default visibility (shown/hidden) set in `PanelData::new()` via `PanelStyle`.

**Persistence caveat:** Panel order AND styles are persisted per-workspace in `db/workspaces/<hash>/workspace_info`. Changes to defaults only take effect for new workspaces or after deleting persisted state.

## Configuration

Config structs in `config/`: `core.rs`, `editor.rs`, `ui.rs`. Defaults in `defaults/settings.toml`.

Keybindings in `defaults/keymaps-{common,macos,nonmacos}.toml`. Each binding has: `key`, `command`, optional `mode` (i=insert, n=normal, v=visual), optional `when` (condition).

User config stored at `Directory::config_directory()` (macOS: `~/Library/Application Support/dev.lapce.{NAME}/`). The app name includes the build type — debug builds use `Lapce-Debug`, release uses `Lapce`.

## Persistence & Data Directory

`LapceDb` in `db.rs` saves state asynchronously via a dedicated thread. Storage at `config_dir/db/`:

- `app` — window positions
- `window` — window layout
- `panel_orders` — global panel arrangement
- `workspaces/<id>/workspace_info` — per-workspace: panel styles, open files, split layout

**Important:** When changing panel defaults, you must delete `db/workspaces/` AND `db/panel_orders` to see changes, because the app loads persisted state over defaults.

## Quirks & Gotchas

- **floem_editor_core re-export:** `lapce-core` does `pub use floem_editor_core::*`. Types like `MultiSelectionCommand`, `EditCommand`, `MoveCommand` originate in floem but are imported via `lapce_core::command`. You cannot import them directly from floem (they're private there).

- **Mode::Terminal exists but is unused:** Defined in floem's `Mode` enum (external dep). Cannot be removed without forking floem. Just ignore it.

- **Build profiles matter:** `cargo build` (dev) and `cargo build --profile fastdev` produce separate artifacts. If you build with one profile but run with another, you get stale code.

- **Debug vs Release app name:** The app name (`Lapce-Debug` vs `Lapce`) is generated at build time in `lapce-core/build.rs` → `meta.rs`. This affects the config/data directory path.

- **Proxy runs in-process for local:** Despite the two-process architecture, local workspaces run the proxy as a thread within the same process (see `proxy.rs`). The separate-process mode was for remote development (now removed).

- **Signal tracking:** Using `signal.get()` inside a view subscribes to updates. Using it in command handlers or initialization should use `get_untracked()` to avoid unnecessary re-renders. Misusing tracked gets in non-view code can cause performance issues.

- **Panel toggle commands:** `toggle_*_focus` shows the panel AND focuses it. `toggle_*_visual` only toggles visibility. The keyboard shortcuts use the `_focus` variants.

- **Strum enum serialization:** Command names in keybindings must match the `#[strum(serialize = "...")]` attribute exactly. The command palette uses `get_message()` for display names.
