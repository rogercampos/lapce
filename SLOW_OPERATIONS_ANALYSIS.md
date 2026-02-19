# Slow Operations Analysis

A comprehensive audit of all operations in Lapce that could exceed 500ms, with special
attention to behavior on very large projects (millions of files, deep directory trees,
monorepos).

---

## Table of Contents

1. [Startup & Initialization](#1-startup--initialization)
2. [Workspace & Project Detection](#2-workspace--project-detection)
3. [Shell Environment Resolution](#3-shell-environment-resolution)
4. [Git Operations](#4-git-operations)
5. [LSP Server Lifecycle](#5-lsp-server-lifecycle)
6. [File System Operations](#6-file-system-operations)
7. [Global Search](#7-global-search)
8. [Syntax Highlighting & Parsing](#8-syntax-highlighting--parsing)
9. [Editor / Document Operations](#9-editor--document-operations)
10. [Configuration Loading](#10-configuration-loading)
11. [Summary Table](#11-summary-table)

---

## 1. Startup & Initialization

### 1.1 Shell Environment Capture (App-level)

| | |
|---|---|
| **File** | `lapce-app/src/app/ipc.rs:16` |
| **Function** | `load_shell_env()` |
| **What** | When launched from a GUI (Finder, Dock), spawns a login shell (`bash --login -c printenv` or equivalent) to capture the full user environment (PATH, etc.). |
| **Why** | Without this, tools like `cargo`, `ruby`, `node` aren't on PATH because GUI apps don't inherit terminal environments. |
| **Why slow** | `Command::output()` (line ~51) blocks synchronously. The login shell sources `.zprofile`, `.bash_profile`, `.zshrc`, etc. Users with version managers (mise, asdf, rbenv, pyenv, nvm) or heavy shell configs can have 1–3 second init times. |
| **Blocking?** | **Yes** — runs on main thread before window creation. |
| **Impact** | 1–3s delay before any window appears on every GUI launch. |

### 1.2 Single-Instance Socket Check

| | |
|---|---|
| **File** | `lapce-app/src/app/ipc.rs:88` |
| **Function** | `try_open_in_existing_process()` |
| **What** | Connects to a local socket to check if another Lapce instance is running. If found, sends file paths to it and exits. |
| **Why** | Prevents duplicate instances; opens files in existing window. |
| **Why slow** | `rx.recv_timeout(Duration::from_millis(500))` (line ~108) blocks up to 500ms waiting for a response from the existing instance. |
| **Blocking?** | **Yes** — main thread, before window creation. |
| **Impact** | Up to 500ms delay if socket exists but instance is busy. |

### 1.3 Grammar Fetching from GitHub

| | |
|---|---|
| **File** | `lapce-app/src/app.rs:1136` (inside `launch()`) |
| **Function** | Background thread calling `find_grammar_release()` (~line 1140) then `fetch_grammars()` |
| **What** | Checks GitHub API for new tree-sitter grammar releases, downloads and extracts them if updated. |
| **Why** | Keeps syntax highlighting grammars up to date without manual action. |
| **Why slow** | HTTP request to GitHub API + potential download of grammar archive. Network-dependent: 1–5s on good connections, timeout on bad. |
| **Blocking?** | No — background thread. |
| **Impact** | Delays availability of syntax highlighting for newly-supported languages until download completes. On offline machines, may timeout. |

### 1.4 Font Loading (vendored-fonts feature)

| | |
|---|---|
| **File** | `lapce-app/src/app.rs:904–977` |
| **What** | Loads 20+ embedded font files into Floem's font system. |
| **Why** | Provides consistent fonts across platforms without system font dependencies. |
| **Why slow** | Synchronous loading/registration of each font file. |
| **Blocking?** | **Yes** — main thread, before rendering. |
| **Impact** | ~100–300ms (typically under threshold, but adds up with other startup costs). |

---

## 2. Workspace & Project Detection

### 2.1 Project Detection via Directory Walk

| | |
|---|---|
| **File** | `lapce-proxy/src/project.rs:10` |
| **Function** | `detect_projects()` |
| **What** | Recursively walks the workspace directory up to **5 levels deep** looking for project marker files (Cargo.toml, Gemfile, package.json, go.mod, pyproject.toml, setup.py, mix.exs, pom.xml, build.gradle, Package.swift). Returns a sorted list of detected projects. |
| **Why** | Needed to know which LSP servers to activate and which shell environments to resolve. |
| **Why slow** | Uses `ignore::WalkBuilder` with `hidden(false)` — walks **every file and directory** up to depth 5, including hidden directories. On a monorepo with dense nesting (e.g. a `vendor/` with thousands of gems, a `node_modules/` at depth 2), this can enumerate millions of entries. |
| **Blocking?** | No — background thread (dispatch.rs ~line 121). But **blocks LSP startup** since LSP activation depends on knowing the projects. |
| **Impact** | **500ms–10s+** on monorepos. A workspace with `node_modules/` containing 500K entries at depth 2 will be traversed fully. |
| **Edge case** | A monorepo with 50 sub-projects at depth 1, each containing their own `node_modules/`, means the walker visits depth 2–5 inside each — potentially millions of entries total. |

---

## 3. Shell Environment Resolution

### 3.1 Default Workspace Environment

| | |
|---|---|
| **File** | `lapce-proxy/src/shell_env.rs:18` (`resolve_shell_env`) and `:31` (`resolve_shell_env_unix`) |
| **Called from** | `lapce-proxy/src/dispatch.rs:141` |
| **What** | Spawns an interactive login shell (`-i -l -c "cd <workspace> && env -0"`) to capture all environment variables, including those set by version managers (mise, asdf, rbenv, rvm, nvm). |
| **Why** | LSP server binaries (e.g. `ruby-lsp`, `bash-language-server`) are often installed in version-manager-managed paths. Without this, they can't be found. |
| **Why slow** | `Command::output()` (line ~62) blocks until the shell finishes. The login shell sources all profile/rc files. On systems with complex shell configs: **1–3 seconds per invocation**. The explicit `cd` into the workspace triggers `chpwd` hooks in tools like mise, which may download/compile tool versions. |
| **Blocking?** | Blocking within its thread. Runs on background thread but **serializes** with subsequent operations. |
| **Impact** | **1–3s** per workspace. |

### 3.2 Per-Project Environment Resolution

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:151–190` |
| **What** | After detecting projects (§2.1), spawns a **separate login shell for each unique project root** to capture project-specific environments. |
| **Why** | Different sub-projects may use different Ruby/Node/Python versions managed by mise/asdf/rbenv. |
| **Why slow** | Same shell spawning cost as §3.1, but **multiplied by number of detected projects**. If a monorepo has 10 sub-projects, this spawns 10 login shells sequentially. |
| **Blocking?** | Same background thread — runs sequentially after §3.1. |
| **Impact** | **1–3s × number of projects**. A workspace with 10 Ruby projects → 10–30s of shell spawning before any LSP server starts. |
| **Edge case** | Projects sharing the same root are not deduplicated; each distinct project path triggers a full shell spawn. A monorepo with 30 microservices = 30 shell invocations. |

---

## 4. Git Operations

### 4.1 Full Git Status Scan

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:1275` |
| **Function** | `read_git_file_statuses()` |
| **What** | Opens the git repository with `git2`, calls `repo.statuses()` with `include_untracked(true)`, `recurse_untracked_dirs(true)`, `include_ignored(true)` (but `recurse_ignored_dirs(false)`). Classifies every file as Modified, Added, Deleted, Untracked, Ignored, Conflicted, or Renamed. Returns a full HashMap. |
| **Why** | Powers the file explorer's git decorations (colored filenames, status indicators). |
| **Why slow** | Scans the **entire working tree** including all untracked files. The `recurse_untracked_dirs(true)` flag means git2 walks into every untracked directory. Even with `recurse_ignored_dirs(false)`, individual ignored files (like `.env`) are still detected. On large repos with many untracked files, this is O(all files in repo). |
| **Blocking?** | Background thread. |
| **Impact** | **500ms–5s+** on repos with >50K files. |
| **Frequency** | Called at initialization (dispatch.rs:113) **AND on every file change event** (dispatch.rs:1238, after 500ms debounce). Saving a file triggers a full re-scan. |

### 4.2 Git Status Re-scan on File Changes

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:1163` |
| **Function** | `handle_workspace_fs_event()` |
| **What** | When the file watcher detects changes, debounces for 500ms, then triggers a **full git status re-scan** (§4.1) plus workspace file change notification. |
| **Why** | Keeps git decorations in sync with the actual working tree. |
| **Why slow** | The debounce adds a **hardcoded 500ms delay** (`thread::sleep(Duration::from_millis(500))` at ~line 1221). After the sleep, runs the full `read_git_file_statuses()` scan again. On large repos, this means every save costs 500ms (debounce) + 1–5s (git scan). |
| **Blocking?** | Background thread. |
| **Impact** | **500ms–6s** latency between saving a file and seeing updated git decorations. During active development with frequent saves, can cause constant background I/O. |
| **Edge case** | Mass file operations (e.g. `git checkout` switching branches, `npm install` creating thousands of files) trigger a firestorm of events, each debounced independently — could queue multiple full scans. |

### 4.3 Git Branch & Repo State Detection

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:1246–1273` |
| **Functions** | `read_git_branch()`, `read_git_repo_state()` |
| **What** | Reads `.git/HEAD` and checks for `.git/rebase-merge`, `.git/rebase-apply` sentinel files. |
| **Why** | Shows current branch name and rebase/merge state in the UI. |
| **Why slow** | Multiple `fs::read_to_string()` and `.exists()` stat calls. Usually fast, but runs synchronously on the dispatcher thread during initialization (line ~90). |
| **Impact** | Normally <50ms. On network filesystems (NFS, SSHFS) could be 100–500ms per stat call. |

---

## 5. LSP Server Lifecycle

### 5.1 LSP Server Process Spawn + Initialize Handshake

| | |
|---|---|
| **File** | `lapce-proxy/src/lsp/client.rs:116` (start), `:250` (initialize) |
| **Function** | `LspClient::start()`, `LspClient::initialize()` |
| **What** | Spawns the LSP server subprocess, creates 3 I/O threads (stdin writer, stdout reader, stderr logger), then performs the synchronous `Initialize` handshake — sends `Initialize` request and **blocks via `rx.recv()`** (line ~311 in `server_request_sync`) waiting for the server's response. |
| **Why** | LSP protocol requires the `Initialize` handshake before any other communication. |
| **Why slow** | `server_request_sync()` uses a blocking channel receive. LSP servers like `ruby-lsp` or `rust-analyzer` can take 1–5+ seconds to initialize (loading project, building indexes). |
| **Blocking?** | **Yes** — blocks the LSP manager thread. No other LSP server can start until this one finishes. |
| **Impact** | **1–5s per language server**. If workspace has Ruby + Bash files, both servers initialize sequentially = 2–10s total. |

### 5.2 Auto-Install: Ruby Gem (`ruby-lsp`)

| | |
|---|---|
| **File** | `lapce-proxy/src/lsp/manager.rs:283` |
| **Function** | `try_gem_install()` |
| **What** | If `ruby-lsp` binary is not found, automatically runs `gem install ruby-lsp` via `Command::output()`. |
| **Why** | Provides zero-config Ruby support — users don't need to manually install the language server. |
| **Why slow** | `gem install` downloads the gem, resolves dependencies, compiles native extensions. This is a **synchronous blocking call** on the LSP manager thread. |
| **Blocking?** | **Yes** — blocks entire LSP manager thread. No LSP servers can start during installation. |
| **Impact** | **10–30s** on first Ruby file open. Network-dependent. |
| **Edge case** | If `gem` command is present but pointing to wrong Ruby version, install can fail after timeout. No explicit timeout set. |

### 5.3 Auto-Install: NPM Package (`bash-language-server`)

| | |
|---|---|
| **File** | `lapce-proxy/src/lsp/manager.rs:324` |
| **Function** | `resolve_npm_server()` |
| **What** | If `bash-language-server` binary is not found in the local servers directory, runs `npm install --prefix <dir> bash-language-server` via `Command::output()`. |
| **Why** | Zero-config Bash support. |
| **Why slow** | Same as §5.2 — `npm install` resolves and downloads packages. Synchronous blocking call. |
| **Blocking?** | **Yes** — blocks LSP manager thread. |
| **Impact** | **5–60s** on first Bash file open. |

### 5.4 didOpen Replay on Late Server Start

| | |
|---|---|
| **File** | `lapce-proxy/src/lsp/manager.rs:404–445` |
| **Function** | `replay_open_files()` |
| **What** | When a server starts after files are already open, fetches content of all open files from the dispatcher and sends `didOpen` notifications for each. |
| **Why** | Ensures the LSP server knows about all currently-open files, not just the one that triggered activation. |
| **Why slow** | `proxy_rpc.get_open_files_content()` fetches content for all open files. With 50+ files open, this involves reading file contents and sending them one by one. |
| **Impact** | **100ms–1s** depending on number of open files. |

---

## 6. File System Operations

### 6.1 Full File Listing (`GetFiles`)

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:620` |
| **What** | Walks the **entire workspace** collecting all file paths. Uses `ignore::WalkBuilder` with `hidden(false)` (includes hidden dirs, excludes `.git/`). Collects ALL paths into a single `Vec<PathBuf>`. |
| **Why** | Powers the file palette (Cmd+P) — needs the full list for fuzzy filtering. |
| **Why slow** | Enumerates every file in the workspace. No streaming, no pagination, no size limit. On a monorepo with 1M+ files (including `node_modules/`, `vendor/`, etc.), this allocates a Vec with 1M+ entries. |
| **Blocking?** | Background thread, but the file palette waits for this result before showing anything. |
| **Impact** | **2–20s+** on monorepos. Users see an empty palette until the full walk completes. |
| **Edge case** | A single `node_modules/` directory can contain 500K+ files. Without filtering, all are collected. |

### 6.2 Directory Listing (`ReadDir`)

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:674` |
| **What** | Reads a single directory's contents. For each entry, calls `e.path().is_dir()` (a stat syscall), then sorts all items. |
| **Why** | Powers the file explorer tree — each directory expansion triggers a ReadDir. |
| **Why slow** | `is_dir()` is an extra stat call per entry. On a directory with 10K+ files (e.g. `node_modules/` root), that's 10K stat calls + sort. No pagination. |
| **Blocking?** | Background thread. |
| **Impact** | **100ms–5s** depending on directory size. Expanding `node_modules/` at the root can freeze the explorer. |

### 6.3 File Explorer Reconciliation

| | |
|---|---|
| **File** | `lapce-app/src/file_explorer/data.rs:224` |
| **Function** | `read_dir_cb()` |
| **What** | When a ReadDir response arrives, reconciles the result with the existing tree state: removes deleted entries, adds new ones, preserves expansion state. Also compiles glob patterns for `files_exclude`. |
| **Why** | Keeps the tree in sync without losing UI state (expanded folders, selections). |
| **Why slow** | Nested loops: for each new item, checks against existing children → O(n×m). Glob pattern compilation happens **on every directory read** (there's a TODO at line ~261: "do not recreate glob every time"). |
| **Impact** | **200–500ms** for directories with 1K+ entries. |

### 6.4 Reveal in File Tree

| | |
|---|---|
| **File** | `lapce-app/src/file_explorer/data.rs:467` |
| **Function** | `reveal_in_file_tree()` |
| **What** | Opens all ancestor directories of a file, reads any unread directories asynchronously, scrolls to and selects the file. |
| **Why** | "Reveal Active File in Explorer" command. |
| **Why slow** | May trigger **cascading async ReadDir calls** for each ancestor directory not yet loaded. For a deeply nested file (`src/app/modules/feature/components/sub/file.rs` = 7 levels), this is 7 sequential ReadDir operations, each with network roundtrip to the proxy. |
| **Impact** | **500ms–2s** for deeply nested files in unloaded directories. |

### 6.5 Buffer Save with Backup Copy

| | |
|---|---|
| **File** | `lapce-proxy/src/buffer.rs:79` |
| **Function** | `Buffer::save()` |
| **What** | Creates a `.bak` copy of the file, writes new content, deletes the backup. Three filesystem operations: copy → write → delete. |
| **Why** | Prevents data loss if a write is interrupted (power loss, crash). |
| **Why slow** | `fs::copy()` duplicates the entire file. For a 100MB file, that's 100MB of I/O before the actual save begins. Especially slow on network/USB storage. |
| **Blocking?** | **Yes** — synchronous in the request handler. Dispatcher cannot process other requests during save. |
| **Impact** | **100ms–5s** depending on file size and storage speed. |

### 6.6 External File Change Detection (`OpenFileChanged`)

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:216–240` |
| **What** | When the file watcher detects a change to an open file, calls `get_mod_time()` (stat) then `load_file()` (full read) to refresh the buffer. |
| **Why** | Keeps editor in sync when external tools modify files (e.g. code generators, formatters). |
| **Why slow** | Full file read into memory. For large generated files (100MB+), noticeable. |
| **Blocking?** | **Yes** — synchronous in notification handler. |
| **Impact** | **50ms–2s** depending on file size. |

---

## 7. Global Search

### 7.1 Proxy-Side Search (`search_in_path`)

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:1331` (called from `GlobalSearch` handler at line 318) |
| **What** | Walks the workspace with `ignore::Walk` (respects .gitignore), then for each file, uses `grep_searcher` to search for the pattern. Supports regex, case sensitivity, whole-word matching. |
| **Why** | Powers both the Search Modal and the Global Search Panel. |
| **Why slow** | Must walk and read **every non-ignored file** in the workspace. No file-type filtering, no file-size limit, no max results. Complex regex patterns can cause exponential backtracking per file. |
| **Blocking?** | Background thread with cancellation token (`WORKER_ID` atomic). UI waits for results. |
| **Impact** | **500ms–60s+** on large workspaces. A Ruby monorepo with 50K source files = 5–15s per search. |
| **Edge case** | Pathological regex (e.g. `.*.*.*`) on large files can cause catastrophic backtracking. No per-file timeout. |

### 7.2 Search Tree Building (UI-side)

| | |
|---|---|
| **File** | `lapce-app/src/global_search.rs:669` |
| **Function** | `build_search_tree()` |
| **What** | Converts flat search results into a hierarchical tree grouped by directory path. Builds nested BTreeMap structure. |
| **Why** | The Global Search Panel shows results in a file tree, not a flat list. |
| **Why slow** | For each result, parses path components and navigates the tree. O(n × m) where n = results, m = average path depth. |
| **Blocking?** | **Yes — runs on UI thread** (inside a `Memo`). |
| **Impact** | **200–500ms** for searches with 5K+ results. Causes UI jank. |

### 7.3 Search Tree Flattening (UI-side)

| | |
|---|---|
| **File** | `lapce-app/src/global_search.rs:751` |
| **Function** | `flatten_tree_entries()` |
| **What** | Recursively flattens the tree into a list of visible rows (respecting expanded/collapsed state), with sorting at each level. |
| **Why** | `virtual_stack` needs a flat list of items to render. |
| **Why slow** | Recursive traversal with sorting (`sorted_keys()`) at each level. Runs in a `Memo` on the UI thread. |
| **Blocking?** | **Yes — UI thread.** |
| **Impact** | **100–300ms** for trees with 5K+ visible rows. |

### 7.4 Search Results Flattening (Search Modal)

| | |
|---|---|
| **File** | `lapce-app/src/search_modal.rs:117–132` |
| **Function** | `flat_matches` memo |
| **What** | Flattens hierarchical search results from `GlobalSearchData` into a flat Vec of `FlatSearchMatch` items. |
| **Why** | The Search Modal displays a flat list (no tree), so it needs to flatten. |
| **Why slow** | Nested iteration through all files and all matches per file. Allocates full result vector. |
| **Blocking?** | **Yes — UI thread** (inside Memo). |
| **Impact** | **50–200ms** for 5K+ matches. |

---

## 8. Syntax Highlighting & Parsing

### 8.1 Tree-Sitter Grammar Loading (Dynamic Library)

| | |
|---|---|
| **File** | `lapce-core/src/language.rs:1316` |
| **What** | Loads tree-sitter grammar from a `.dylib`/`.so`/`.dll` file using `libloading::Library::new()` (line ~1338). Then `std::mem::forget(library)` to keep function pointers valid for the process lifetime. |
| **Why** | Each language needs its tree-sitter grammar for syntax highlighting, bracket matching, and code navigation. |
| **Why slow** | Dynamic library loading involves disk I/O, memory mapping, and symbol resolution. First load for each language. |
| **Blocking?** | **Yes** — happens on whatever thread first requests highlighting for that language. |
| **Impact** | **10–100ms** per grammar. Not individually over 500ms, but if opening a project with many languages simultaneously, cumulative cost adds up. |

### 8.2 HighlightConfiguration Creation

| | |
|---|---|
| **File** | `lapce-core/src/syntax/highlight.rs:143–238` |
| **What** | Parses tree-sitter query files into compiled query objects. Cached per-thread in thread-local storage. |
| **Why** | Query files define how AST nodes map to syntax highlight tokens. |
| **Why slow** | `Query::new()` parses and compiles the query language. Complex queries (e.g. Rust has hundreds of rules) take longer. Query files may use `;; inherits:` directives requiring recursive file reads (language.rs:1475). |
| **Blocking?** | **Yes** — on the thread that needs it. |
| **Impact** | **50–200ms** per language, first time only. |

### 8.3 Syntax Tree Parsing

| | |
|---|---|
| **File** | `lapce-core/src/syntax/mod.rs:413–447` |
| **What** | Parses source code into a tree-sitter AST via `parser.parse_with()`. |
| **Why** | Required for syntax highlighting, bracket matching, and code navigation. |
| **Why slow** | Full AST construction. Has a **hardcoded 500ms timeout** (line 641: `ts_parser.parser.set_timeout_micros(1000 * 500)`). For very large files or complex grammars, parsing hits this timeout and aborts — leaving the file without syntax highlighting. |
| **Blocking?** | Runs on rayon thread pool (doc.rs:732, `trigger_syntax_change()`). |
| **Impact** | **100–500ms** (hard-capped at 500ms by timeout). Files >100KB with complex grammars (e.g. deeply nested HTML with JS injections) will frequently hit the cap. |

### 8.4 Injection Layer Discovery

| | |
|---|---|
| **File** | `lapce-core/src/syntax/mod.rs:504–818` |
| **Function** | `SyntaxLayers::update()` |
| **What** | After parsing the main tree, discovers injection points (e.g. `<script>` tags in HTML → JavaScript parser, template strings → embedded languages). Creates/reuses injection layers, each with its own parse tree. |
| **Why** | Multi-language files (HTML, JSX, Markdown with code blocks) need multiple parsers. |
| **Why slow** | Runs injection queries on the entire tree, hashes results for layer reuse, parses each new injection layer. Complex files with many injections can have dozens of layers. |
| **Blocking?** | Same rayon thread as §8.3. |
| **Impact** | **50–500ms** for complex multi-language files. An HTML file with 20 `<script>` tags = 20 JS injection layers to parse. |

### 8.5 Bracket Colorization (Naive Parser)

| | |
|---|---|
| **File** | `lapce-core/src/syntax/mod.rs:174–234` |
| **What** | For rainbow bracket colorization, scans the entire file character-by-character to find and match brackets. Falls back to this naive approach when tree-sitter doesn't support the language. |
| **Why** | Provides bracket matching even for unsupported languages. |
| **Why slow** | O(file_size) character scan. The tree-sitter-aware version (`walk_tree_bracket_ast`) is faster for supported languages, but the fallback path scans raw text. |
| **Impact** | **100–500ms** for files >100KB using the fallback parser. |

---

## 9. Editor / Document Operations

### 9.1 Semantic Token Processing

| | |
|---|---|
| **File** | `lapce-app/src/doc.rs:817` |
| **Function** | `get_semantic_styles()` |
| **What** | When the LSP sends semantic tokens, spawns a thread to build `Spans<Style>` from the token data. Iterates all tokens, maps them to styles. |
| **Why** | Semantic tokens provide more accurate coloring than tree-sitter (e.g. distinguishing local variables from parameters). |
| **Why slow** | `SpansBuilder::new(len)` allocates for the full document. Then iterates all tokens. For files with 10K+ semantic tokens (large source files), this is noticeable. |
| **Blocking?** | Background thread, but result is set on a signal that triggers re-render. |
| **Impact** | **100–500ms** for large files with dense semantic tokens. |

### 9.2 Phantom Text Assembly (Per Line)

| | |
|---|---|
| **File** | `lapce-app/src/doc.rs:1414` |
| **Function** | `phantom_text()` (DocumentPhantom trait) |
| **What** | For each line, collects all phantom text items: inlay hints, error lens diagnostics, completion lens, inline completion, IME preedit. Sorts them by column. |
| **Why** | Phantom text is virtual text rendered inline (type hints, error messages, completions). |
| **Why slow** | Calls `iter_chunks()` multiple times on inlay hints and diagnostics spans. Sorts results. Not individually slow, but runs **per visible line** during rendering. With 50 visible lines and hundreds of hints, it accumulates. |
| **Blocking?** | **Yes — UI thread** during paint. |
| **Impact** | **50–200ms total** across all visible lines in files with dense inlay hints. |

### 9.3 Go-to-Definition with Fallback

| | |
|---|---|
| **File** | `lapce-app/src/editor.rs:902` |
| **Function** | `go_to_definition()` |
| **What** | Requests definition from LSP. If the result points to the current location (already at definition), falls back to requesting references instead. Deduplicates results. Has Ruby-specific filtering logic. |
| **Why** | When user is on a definition site, "go to definition" should show usages instead. |
| **Why slow** | Can make **two sequential LSP requests** (definition → references) if the first returns the current location. Each request depends on LSP server response time. |
| **Blocking?** | Async callbacks, but user perceives latency. |
| **Impact** | **500ms–5s** when fallback triggers, depending on LSP server speed and project size. |

### 9.4 Code Actions on Cursor Move

| | |
|---|---|
| **File** | `lapce-app/src/editor.rs:1714` |
| **Function** | `get_code_actions()` |
| **What** | Fetches available code actions (quick fixes, refactorings) for the current cursor position. Builds diagnostics list from spans. |
| **Why** | Shows the lightbulb icon and available fixes. |
| **Why slow** | LSP request is made on **every cursor position change**. Slow LSP servers can take 500ms+ to respond. Building the diagnostics list involves iterating spans. |
| **Blocking?** | Async, but fires very frequently. |
| **Impact** | **100ms–1s** per cursor move. Can overload slow LSP servers with rapid cursor movements. |

### 9.5 Find References Resolution

| | |
|---|---|
| **File** | `lapce-proxy/src/dispatch.rs:1051` |
| **Function** | `ReferencesResolve` handler |
| **What** | For each reference location, loads the file if not already open via `get_buffer_or_insert()`, then extracts the line content. |
| **Why** | Shows reference context (the actual line of code) in the references panel. |
| **Why slow** | If references span 50 different files, loads 50 files from disk sequentially. No batching or caching. |
| **Blocking?** | **Yes** — synchronous in request handler. |
| **Impact** | **500ms–10s** for references across many files (e.g. a widely-used utility function). |

### 9.6 Completion & Inline Completion Updates

| | |
|---|---|
| **File** | `lapce-app/src/editor.rs:1282–1384` |
| **What** | On every keystroke, may trigger both LSP completion request and inline completion request. |
| **Why** | Auto-complete as you type. |
| **Why slow** | LSP completion requests depend on server response time. Multiple requests can queue up during fast typing. |
| **Impact** | **100–500ms** per keystroke in perceived lag, depending on LSP server. |

---

## 10. Configuration Loading

### 10.1 Multi-Layer Config Merge

| | |
|---|---|
| **File** | `lapce-app/src/config.rs:126` |
| **Function** | `LapceConfig::load()` |
| **What** | Merges 6 layers of configuration: embedded defaults → dark theme base → active color theme → icon theme → user global settings → workspace-local settings. Parses TOML, resolves `$variable` color references recursively (max depth 6), loads icon themes. |
| **Why** | Supports the full configuration override chain. |
| **Why slow** | Multiple TOML parsing operations, recursive color variable resolution, file I/O for user/workspace settings. |
| **Blocking?** | **Yes — main thread** during startup. |
| **Impact** | **100–300ms per call**. Called **3 times during startup**: (1) in `app.rs` for window scale, (2) in `window.rs` for window config, (3) in `workspace_data.rs` per workspace. Total: **300–900ms**. |
| **Edge case** | User settings file with many overrides, or a complex custom theme, can increase parsing time. |

---

## 11. Summary Table

### By Estimated Duration (worst case, large projects)

| # | Operation | Duration | Blocking UI? | Frequency | Location |
|---|-----------|----------|:---:|-----------|----------|
| 5.2 | Gem auto-install (ruby-lsp) | 10–30s | No* | Once | lsp/manager.rs:283 |
| 5.3 | NPM auto-install (bash-language-server) | 5–60s | No* | Once | lsp/manager.rs:324 |
| 3.2 | Per-project shell env resolution | 1–3s × N projects | No* | Init | dispatch.rs:151 |
| 6.1 | Full file listing (GetFiles) | 2–20s | No** | Palette open | dispatch.rs:620 |
| 7.1 | Global search (search_in_path) | 500ms–60s | No** | Per search | dispatch.rs:1331 |
| 4.1 | Full git status scan | 500ms–5s | No | Init + saves | dispatch.rs:1275 |
| 2.1 | Project detection walk | 500ms–10s | No* | Init | project.rs:10 |
| 3.1 | Default shell env resolution | 1–3s | No* | Init | shell_env.rs:18 |
| 5.1 | LSP Initialize handshake | 1–5s | No* | Per language | lsp/client.rs:116 |
| 1.1 | Shell env capture (app-level) | 1–3s | **Yes** | GUI launch | app/ipc.rs:16 |
| 9.5 | References resolve (file loading) | 500ms–10s | No*** | User action | dispatch.rs:1051 |
| 9.3 | Go-to-definition fallback | 500ms–5s | No | User action | editor.rs:902 |
| 10.1 | Config loading (×3) | 300–900ms total | **Yes** | Startup | config.rs:126 |
| 4.2 | Git status re-scan on changes | 500ms debounce + 500ms–5s | No | Every save | dispatch.rs:1163 |
| 8.3 | Syntax tree parsing | 100–500ms (capped) | No | File open/edit | syntax/mod.rs:413 |
| 6.5 | Buffer save with backup | 100ms–5s | No*** | Every save | buffer.rs:79 |
| 6.2 | ReadDir (large directories) | 100ms–5s | No | Tree expand | dispatch.rs:674 |
| 7.2 | Search tree building (UI) | 200–500ms | **Yes** | Per search | global_search.rs:669 |
| 6.4 | Reveal in file tree | 500ms–2s | No | User action | file_explorer/data.rs:467 |
| 1.2 | Single-instance socket check | up to 500ms | **Yes** | Startup | app/ipc.rs:88 |
| 8.4 | Injection layer discovery | 50–500ms | No | Multi-lang files | syntax/mod.rs:504 |
| 9.1 | Semantic token processing | 100–500ms | No | LSP response | doc.rs:817 |
| 7.3 | Search tree flattening (UI) | 100–300ms | **Yes** | Per search | global_search.rs:751 |
| 9.2 | Phantom text assembly | 50–200ms total | **Yes** | Every render | doc.rs:1414 |
| 8.1 | Grammar dynamic loading | 10–100ms each | Yes (first time) | Per language | language.rs:1316 |
| 8.2 | Highlight config creation | 50–200ms each | Yes (first time) | Per language | highlight.rs:143 |

**Legend:**
- \* Runs on background thread but **blocks LSP startup** (user sees no language features until complete)
- \** Runs on background thread but **UI waits** for results before showing content
- \*** Runs synchronously on dispatcher thread — **blocks all other proxy requests** during execution

### Startup Timeline (large monorepo with Ruby + Bash, 10 sub-projects)

```
T+0s     App launch
         ├── Shell env capture (app-level)     ██████████ 1-3s  [BLOCKING UI]
         ├── Single-instance check              █████ 0-500ms    [BLOCKING UI]
         └── Config loading (×3)                ██████ 300-900ms [BLOCKING UI]
                                                                 ← Window appears
T+1-4s   Proxy initialization (background)
         ├── Git status scan                    ██████████ 500ms-5s
         ├── Project detection                  ████████████ 500ms-10s
         ├── Shell env (default)                ██████████ 1-3s
         ├── Shell env (×10 projects)           ████████████████████████ 10-30s
         │                                                       ← Projects/env ready
         └── LSP startup
             ├── gem install ruby-lsp           ████████████████████ 10-30s (first time)
             ├── npm install bash-lsp           ████████████████████ 5-60s (first time)
             ├── ruby-lsp Initialize            ██████ 1-5s
             └── bash-lsp Initialize            ████ 1-3s
                                                                 ← Language features ready
T+15-90s  Full initialization complete (first time with installs)
T+5-40s   Full initialization complete (subsequent launches)
```

### Critical Architectural Issues

1. **Sequential shell spawning**: Per-project env resolution runs sequentially, not in parallel. 10 projects = 10× the cost, not a constant.

2. **No incremental git status**: Every file change triggers a full repo scan. Should use watcher events to update incrementally.

3. **GetFiles loads everything into memory**: The file palette requires the complete file list before showing. Should stream results incrementally.

4. **Search tree building on UI thread**: `build_search_tree()` and `flatten_tree_entries()` run in Memos on the UI thread. Should be computed off-thread.

5. **LSP installs block the LSP manager thread**: While `gem install` runs, no other language server can start — even for unrelated languages.

6. **No request timeout on shell spawning**: `resolve_shell_env_unix()` calls `Command::output()` with no timeout. A hanging shell (waiting for input, slow network mount) blocks forever.

7. **Glob recompilation on every ReadDir**: The `files_exclude` glob is recompiled on every directory read instead of being cached (there's a TODO in the code acknowledging this).

8. **Dispatcher processes requests sequentially**: While most heavy operations spawn threads, some synchronous operations (buffer save, file change detection, references resolve) block the entire request queue.
