# SourceDelve

## Prerequisites

- **Rust toolchain** via [rustup](https://rustup.rs/) (minimum version: 1.87.0)
- **macOS** for `.app` bundle generation (the Makefile uses macOS-specific tools)

## Development

The `fastdev` profile compiles dependencies with optimizations but keeps your code in debug mode, giving fast iteration with good performance:

```bash
cargo build --profile fastdev
cargo run --profile fastdev --bin sourcedelve
```

### Running CI checks locally

```bash
make ci
```

This runs formatting check, clippy, build, and doc tests.

### Testing

```bash
# Run all tests (unit + integration)
cargo test --workspace

# Run only doc tests
cargo test --doc --workspace
```

### Code coverage

Requires [cargo-llvm-cov](https://github.com/tauri-apps/cargo-llvm-cov) (`cargo install cargo-llvm-cov`).

```bash
# Terminal summary
make coverage-summary

# HTML report (opens target/llvm-cov/html/index.html)
make coverage
```

### Formatting

Always run after making code changes:

```bash
cargo fmt --all
```

## Building the macOS .app bundle

### 1. Native build (current architecture only)

```bash
make app
```

This will:

1. Compile a release build with LTO (`cargo build --profile release-lto`)
2. Assemble the `SourceDelve.app` bundle from the template in `extra/macos/`
3. Codesign the app

The resulting `.app` is at: **`target/release-lto/macos/SourceDelve.app`**

### 2. Universal build (x86_64 + aarch64)

```bash
make app-universal
```

Same as above but builds for both Intel and Apple Silicon, combining them with `lipo` into a universal binary.

### 3. DMG disk image

```bash
make dmg            # native architecture
make dmg-universal  # universal binary
```

The `.dmg` is at: **`target/release-lto/macos/SourceDelve.dmg`**

### Codesigning

The Makefile expects an Apple Developer signing identity matching the fingerprint in `CODESIGN_IDENTITY`. If you don't have one, the build will succeed but codesigning will fail at the end. The unsigned `.app` is still usable — you may need to clear the quarantine attribute:

```bash
xattr -cr target/release-lto/macos/SourceDelve.app
```

To use your own identity, edit `CODESIGN_IDENTITY` in the Makefile.

## Build profiles

| Profile | Command | Use case |
|---------|---------|----------|
| `fastdev` | `cargo build --profile fastdev` | Daily development. Debug mode for workspace code, optimized dependencies |
| `release` | `cargo build --release` | Optimized build without LTO |
| `release-lto` | `cargo build --profile release-lto` | Production build with LTO and single codegen unit (used by `make app`) |

## Project structure

```
lapce/
├── lapce-app/     UI application (Floem framework)
├── lapce-proxy/   Backend process for LSP, file I/O
├── lapce-rpc/     RPC types shared between app and proxy
├── lapce-core/    Core types (rope, commands, cursor, syntax)
└── floem-local/   Local fork of the Floem UI framework
```

## License

Apache License Version 2. See [LICENSE](LICENSE).
