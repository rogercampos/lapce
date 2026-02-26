use std::{
    borrow::Cow,
    fs,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Result, anyhow};
use floem_editor_core::buffer::rope_text::{CharIndicesJoin, RopeTextRef};
use lapce_core::{encoding::offset_utf8_to_utf16, rope_text_pos::RopeTextPosition};
use lapce_rpc::buffer::BufferId;
use lapce_xi_rope::{RopeDelta, interval::IntervalBounds, rope::Rope};
use lsp_types::*;

#[derive(Clone)]
pub struct Buffer {
    pub language_id: &'static str,
    pub read_only: bool,
    pub id: BufferId,
    pub rope: Rope,
    pub path: PathBuf,
    pub rev: u64,
    pub mod_time: Option<SystemTime>,
}

impl Buffer {
    /// Creates a buffer by loading a file from disk. Error handling is intentionally
    /// permissive: instead of propagating errors, the buffer is created with a
    /// placeholder message. This allows the editor to show "Permission Denied" etc.
    /// inline rather than crashing.  NotFound is treated as a new (empty) file.
    pub fn new(id: BufferId, path: PathBuf) -> Buffer {
        let (s, read_only) = match load_file(&path) {
            Ok(s) => (s, false),
            Err(err) => {
                use std::io::ErrorKind;
                match err.downcast_ref::<std::io::Error>() {
                    Some(err) => match err.kind() {
                        ErrorKind::PermissionDenied => {
                            ("Permission Denied".to_string(), true)
                        }
                        ErrorKind::NotFound => ("".to_string(), false),
                        ErrorKind::OutOfMemory => {
                            ("File too big (out of memory)".to_string(), false)
                        }
                        _ => (format!("Not supported: {err}"), true),
                    },
                    None => (format!("Not supported: {err}"), true),
                }
            }
        };
        let rope = Rope::from(s);
        // Start revision at 1 for non-empty files, 0 for empty. This ensures the
        // first edit to a new (empty) file produces rev=1, while existing files
        // that the user hasn't modified yet already have rev=1.
        let rev = u64::from(!rope.is_empty());
        let language_id = language_id_from_path(&path).unwrap_or("");
        let mod_time = get_mod_time(&path);
        Buffer {
            id,
            rope,
            read_only,
            path,
            language_id,
            rev,
            mod_time,
        }
    }

    /// Saves the buffer to disk using an atomic write-to-temp-then-rename
    /// strategy:
    /// 1. Write new content to a temporary file in the same directory
    /// 2. Sync to disk
    /// 3. Atomically rename over the original
    ///
    /// On crash, either the old file or the new file exists — never a
    /// corrupted mix. For symlinks, we write through to the real path so the
    /// link is preserved.
    ///
    /// The `rev` check ensures we don't save stale content if the buffer was
    /// modified between the save request being sent and arriving here.
    pub fn save(&mut self, rev: u64, create_parents: bool) -> Result<()> {
        if self.read_only {
            return Err(anyhow!("can't save to read only file"));
        }

        if self.rev != rev {
            return Err(anyhow!("not the right rev"));
        }

        // Resolve symlinks so we write to the actual file, not replace the
        // symlink with a regular file (which would break the link).
        let path = if self.path.is_symlink() {
            self.path.canonicalize()?
        } else {
            self.path.clone()
        };

        if create_parents {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
        }

        // Write to a temp file in the same directory (ensures same filesystem
        // for atomic rename).
        let tmp_path = path.with_extension("lapce-tmp");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        for chunk in self.rope.iter_chunks(..self.rope.len()) {
            f.write_all(chunk.as_bytes())?;
        }
        f.sync_all()?;

        fs::rename(&tmp_path, &path)?;
        self.mod_time = get_mod_time(&path);

        Ok(())
    }

    /// Applies a text edit delta to the buffer. Returns an incremental LSP content
    /// change event if the delta is a simple insert or delete; otherwise returns a
    /// full-document change (None -> caller falls back to full text).
    ///
    /// The rev check enforces strict sequential ordering -- if edits arrive out of
    /// order, we reject them. This can happen if the UI sends multiple rapid edits
    /// and one gets reordered.
    pub fn update(
        &mut self,
        delta: &RopeDelta,
        rev: u64,
    ) -> Option<TextDocumentContentChangeEvent> {
        if self.rev + 1 != rev {
            tracing::warn!(
                "Out-of-order edit for {:?}: expected rev {}, got {}",
                self.path,
                self.rev + 1,
                rev
            );
            return None;
        }
        self.rev += 1;
        let content_change = get_document_content_change(&self.rope, delta);
        self.rope = delta.apply(&self.rope);
        Some(
            content_change.unwrap_or_else(|| TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: self.get_document(),
            }),
        )
    }

    pub fn get_document(&self) -> String {
        self.rope.to_string()
    }

    pub fn offset_of_line(&self, line: usize) -> usize {
        self.rope.offset_of_line(line)
    }

    pub fn line_of_offset(&self, offset: usize) -> usize {
        self.rope.line_of_offset(offset)
    }

    pub fn offset_to_line_col(&self, offset: usize) -> (usize, usize) {
        let line = self.line_of_offset(offset);
        (line, offset - self.offset_of_line(line))
    }

    /// Converts a UTF8 offset to a UTF16 LSP position  
    pub fn offset_to_position(&self, offset: usize) -> Position {
        let (line, col) = self.offset_to_line_col(offset);
        // Get the offset of line to make the conversion cheaper, rather than working
        // from the very start of the document to `offset`
        let line_offset = self.offset_of_line(line);
        let utf16_col =
            offset_utf8_to_utf16(self.char_indices_iter(line_offset..), col);

        Position {
            line: line as u32,
            character: utf16_col as u32,
        }
    }

    pub fn slice_to_cow<T: IntervalBounds>(&self, range: T) -> Cow<'_, str> {
        self.rope.slice_to_cow(range)
    }

    pub fn line_to_cow(&self, line: usize) -> Cow<'_, str> {
        self.rope
            .slice_to_cow(self.offset_of_line(line)..self.offset_of_line(line + 1))
    }

    /// Iterate over (utf8_offset, char) values in the given range  
    /// This uses `iter_chunks` and so does not allocate, compared to `slice_to_cow` which can
    pub fn char_indices_iter<T: IntervalBounds>(
        &self,
        range: T,
    ) -> impl Iterator<Item = (usize, char)> + '_ {
        CharIndicesJoin::new(self.rope.iter_chunks(range).map(str::char_indices))
    }

    pub fn len(&self) -> usize {
        self.rope.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn load_file(path: &Path) -> Result<String> {
    read_path_to_string(path)
}

pub fn read_path_to_string<P: AsRef<Path>>(path: P) -> Result<String> {
    let path = path.as_ref();

    let mut file = File::open(path)?;
    // Read the file in as bytes
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    // Parse the file contents as utf8
    let contents = String::from_utf8(buffer)?;

    Ok(contents.to_string())
}

pub fn language_id_from_path(path: &Path) -> Option<&'static str> {
    // recommended language_id values
    // https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocumentItem
    Some(match path.extension() {
        Some(ext) => {
            match ext.to_str()? {
                "C" | "H" => "cpp",
                "M" => "objective-c",
                // stop case-sensitive matching
                ext => match ext.to_lowercase().as_str() {
                    "bat" => "bat",
                    "clj" | "cljs" | "cljc" | "edn" => "clojure",
                    "coffee" => "coffeescript",
                    "c" | "h" => "c",
                    "cpp" | "hpp" | "cxx" | "hxx" | "c++" | "h++" | "cc" | "hh" => {
                        "cpp"
                    }
                    "cs" | "csx" => "csharp",
                    "css" => "css",
                    "d" | "di" | "dlang" => "dlang",
                    "diff" | "patch" => "diff",
                    "dart" => "dart",
                    "dockerfile" => "dockerfile",
                    "elm" => "elm",
                    "ex" | "exs" => "elixir",
                    "erl" | "hrl" => "erlang",
                    "fs" | "fsi" | "fsx" | "fsscript" => "fsharp",
                    "git-commit" | "git-rebase" => "git",
                    "go" => "go",
                    "groovy" | "gvy" | "gy" | "gsh" => "groovy",
                    "hbs" => "handlebars",
                    "htm" | "html" | "xhtml" => "html",
                    "ini" => "ini",
                    "java" | "class" => "java",
                    "js" => "javascript",
                    "jsx" => "javascriptreact",
                    "json" => "json",
                    "jl" => "julia",
                    "kt" => "kotlin",
                    "kts" => "kotlinbuildscript",
                    "less" => "less",
                    "lua" => "lua",
                    "makefile" | "gnumakefile" => "makefile",
                    "md" | "markdown" => "markdown",
                    "m" => "objective-c",
                    "mm" => "objective-cpp",
                    "plx" | "pl" | "pm" | "xs" | "t" | "pod" | "cgi" => "perl",
                    "p6" | "pm6" | "pod6" | "t6" | "raku" | "rakumod"
                    | "rakudoc" | "rakutest" => "perl6",
                    "php" | "phtml" | "pht" | "phps" => "php",
                    "proto" => "proto",
                    "ps1" | "ps1xml" | "psc1" | "psm1" | "psd1" | "pssc"
                    | "psrc" => "powershell",
                    "py" | "pyi" | "pyc" | "pyd" | "pyw" => "python",
                    "r" => "r",
                    "rb" => "ruby",
                    "rs" => "rust",
                    "scss" | "sass" => "scss",
                    "sc" | "scala" => "scala",
                    "sh" | "bash" | "zsh" => "shellscript",
                    "sql" => "sql",
                    "swift" => "swift",
                    "svelte" => "svelte",
                    "thrift" => "thrift",
                    "toml" => "toml",
                    "ts" => "typescript",
                    "tsx" => "typescriptreact",
                    "tex" => "tex",
                    "vb" => "vb",
                    "xml" | "csproj" => "xml",
                    "xsl" => "xsl",
                    "yml" | "yaml" => "yaml",
                    "zig" => "zig",
                    "vue" => "vue",
                    _ => return None,
                },
            }
        }
        // Handle paths without extension
        #[allow(clippy::match_single_binding)]
        None => match path.file_name()?.to_str()? {
            // case-insensitive matching
            filename => match filename.to_lowercase().as_str() {
                "dockerfile" => "dockerfile",
                "makefile" | "gnumakefile" => "makefile",
                _ => return None,
            },
        },
    })
}

/// Attempts to compute an incremental LSP TextDocumentContentChangeEvent from a
/// rope delta. Only handles the two most common cases: simple insert and simple
/// delete. More complex edits (e.g., replace, transpose) fall through to None,
/// causing the caller to send the full document text instead.
pub(crate) fn get_document_content_change(
    rope: &Rope,
    delta: &RopeDelta,
) -> Option<TextDocumentContentChangeEvent> {
    let (interval, _) = delta.summary();
    let (start, end) = interval.start_end();
    let text = RopeTextRef::new(rope);

    if let Some(node) = delta.as_simple_insert() {
        let (start, end) = interval.start_end();
        let start = text.offset_to_position(start);
        let end = text.offset_to_position(end);

        Some(TextDocumentContentChangeEvent {
            range: Some(Range { start, end }),
            range_length: None,
            text: String::from(node),
        })
    } else if delta.is_simple_delete() {
        let end_position = text.offset_to_position(end);
        let start = text.offset_to_position(start);

        Some(TextDocumentContentChangeEvent {
            range: Some(Range {
                start,
                end: end_position,
            }),
            range_length: None,
            text: String::new(),
        })
    } else {
        None
    }
}

/// Returns the modification timestamp for the file at a given path,
/// if present.
pub fn get_mod_time<P: AsRef<Path>>(path: P) -> Option<SystemTime> {
    File::open(path)
        .and_then(|f| f.metadata())
        .and_then(|meta| meta.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_id_rust() {
        assert_eq!(language_id_from_path(Path::new("main.rs")), Some("rust"));
    }

    #[test]
    fn language_id_python() {
        assert_eq!(
            language_id_from_path(Path::new("script.py")),
            Some("python")
        );
        assert_eq!(language_id_from_path(Path::new("stub.pyi")), Some("python"));
    }

    #[test]
    fn language_id_javascript_and_typescript() {
        assert_eq!(
            language_id_from_path(Path::new("app.js")),
            Some("javascript")
        );
        assert_eq!(
            language_id_from_path(Path::new("app.jsx")),
            Some("javascriptreact")
        );
        assert_eq!(
            language_id_from_path(Path::new("app.ts")),
            Some("typescript")
        );
        assert_eq!(
            language_id_from_path(Path::new("app.tsx")),
            Some("typescriptreact")
        );
    }

    #[test]
    fn language_id_go() {
        assert_eq!(language_id_from_path(Path::new("main.go")), Some("go"));
    }

    #[test]
    fn language_id_c_and_cpp() {
        assert_eq!(language_id_from_path(Path::new("main.c")), Some("c"));
        assert_eq!(language_id_from_path(Path::new("header.h")), Some("c"));
        assert_eq!(language_id_from_path(Path::new("main.cpp")), Some("cpp"));
        assert_eq!(language_id_from_path(Path::new("header.hpp")), Some("cpp"));
        assert_eq!(language_id_from_path(Path::new("main.cc")), Some("cpp"));
        assert_eq!(language_id_from_path(Path::new("header.hh")), Some("cpp"));
    }

    #[test]
    fn language_id_case_sensitive_uppercase_c_h() {
        // Uppercase .C and .H are treated as C++ (case-sensitive branch)
        assert_eq!(language_id_from_path(Path::new("main.C")), Some("cpp"));
        assert_eq!(language_id_from_path(Path::new("header.H")), Some("cpp"));
    }

    #[test]
    fn language_id_case_sensitive_uppercase_m() {
        // Uppercase .M is objective-c (case-sensitive branch)
        assert_eq!(
            language_id_from_path(Path::new("file.M")),
            Some("objective-c")
        );
    }

    #[test]
    fn language_id_case_insensitive_lowercase_extensions() {
        // lowercase .rs should match "rust" via the case-insensitive branch
        assert_eq!(language_id_from_path(Path::new("foo.RS")), Some("rust"));
        assert_eq!(language_id_from_path(Path::new("foo.Py")), Some("python"));
        assert_eq!(language_id_from_path(Path::new("foo.Go")), Some("go"));
    }

    #[test]
    fn language_id_various_languages() {
        assert_eq!(language_id_from_path(Path::new("style.css")), Some("css"));
        assert_eq!(language_id_from_path(Path::new("page.html")), Some("html"));
        assert_eq!(language_id_from_path(Path::new("data.json")), Some("json"));
        assert_eq!(
            language_id_from_path(Path::new("config.yaml")),
            Some("yaml")
        );
        assert_eq!(language_id_from_path(Path::new("config.yml")), Some("yaml"));
        assert_eq!(
            language_id_from_path(Path::new("config.toml")),
            Some("toml")
        );
        assert_eq!(
            language_id_from_path(Path::new("readme.md")),
            Some("markdown")
        );
        assert_eq!(language_id_from_path(Path::new("query.sql")), Some("sql"));
        assert_eq!(language_id_from_path(Path::new("script.rb")), Some("ruby"));
        assert_eq!(language_id_from_path(Path::new("main.java")), Some("java"));
        assert_eq!(language_id_from_path(Path::new("main.kt")), Some("kotlin"));
        assert_eq!(
            language_id_from_path(Path::new("main.swift")),
            Some("swift")
        );
        assert_eq!(language_id_from_path(Path::new("main.dart")), Some("dart"));
        assert_eq!(language_id_from_path(Path::new("main.lua")), Some("lua"));
        assert_eq!(language_id_from_path(Path::new("main.zig")), Some("zig"));
        assert_eq!(language_id_from_path(Path::new("app.vue")), Some("vue"));
        assert_eq!(
            language_id_from_path(Path::new("app.svelte")),
            Some("svelte")
        );
        assert_eq!(
            language_id_from_path(Path::new("script.sh")),
            Some("shellscript")
        );
        assert_eq!(
            language_id_from_path(Path::new("script.bash")),
            Some("shellscript")
        );
        assert_eq!(
            language_id_from_path(Path::new("script.zsh")),
            Some("shellscript")
        );
    }

    #[test]
    fn language_id_filename_based_dockerfile() {
        assert_eq!(
            language_id_from_path(Path::new("Dockerfile")),
            Some("dockerfile")
        );
        // Case-insensitive filename match
        assert_eq!(
            language_id_from_path(Path::new("dockerfile")),
            Some("dockerfile")
        );
    }

    #[test]
    fn language_id_filename_based_makefile() {
        assert_eq!(
            language_id_from_path(Path::new("Makefile")),
            Some("makefile")
        );
        assert_eq!(
            language_id_from_path(Path::new("makefile")),
            Some("makefile")
        );
        assert_eq!(
            language_id_from_path(Path::new("GNUmakefile")),
            Some("makefile")
        );
    }

    #[test]
    fn language_id_extension_based_dockerfile_makefile() {
        // .dockerfile extension also maps to dockerfile
        assert_eq!(
            language_id_from_path(Path::new("my.dockerfile")),
            Some("dockerfile")
        );
        // .makefile extension also maps to makefile
        assert_eq!(
            language_id_from_path(Path::new("my.makefile")),
            Some("makefile")
        );
    }

    #[test]
    fn language_id_unknown_extension() {
        assert_eq!(language_id_from_path(Path::new("file.xyz")), None);
        assert_eq!(language_id_from_path(Path::new("file.unknown")), None);
    }

    #[test]
    fn language_id_unknown_filename_no_extension() {
        assert_eq!(language_id_from_path(Path::new("SOMEFILE")), None);
    }

    #[test]
    fn language_id_with_directory_path() {
        assert_eq!(
            language_id_from_path(Path::new("/home/user/project/src/main.rs")),
            Some("rust")
        );
    }

    #[test]
    fn language_id_objective_cpp() {
        assert_eq!(
            language_id_from_path(Path::new("file.mm")),
            Some("objective-cpp")
        );
        // lowercase .m is objective-c
        assert_eq!(
            language_id_from_path(Path::new("file.m")),
            Some("objective-c")
        );
    }

    #[test]
    fn language_id_clojure() {
        assert_eq!(
            language_id_from_path(Path::new("core.clj")),
            Some("clojure")
        );
        assert_eq!(
            language_id_from_path(Path::new("core.cljs")),
            Some("clojure")
        );
        assert_eq!(
            language_id_from_path(Path::new("core.edn")),
            Some("clojure")
        );
    }

    #[test]
    fn language_id_elixir_erlang() {
        assert_eq!(language_id_from_path(Path::new("lib.ex")), Some("elixir"));
        assert_eq!(language_id_from_path(Path::new("test.exs")), Some("elixir"));
        assert_eq!(language_id_from_path(Path::new("lib.erl")), Some("erlang"));
    }

    #[test]
    fn language_id_scss_sass() {
        assert_eq!(language_id_from_path(Path::new("style.scss")), Some("scss"));
        assert_eq!(language_id_from_path(Path::new("style.sass")), Some("scss"));
    }

    // --- get_document_content_change tests ---

    use lapce_xi_rope::{Delta, Interval, rope::Rope as XiRope};

    fn make_rope(s: &str) -> XiRope {
        XiRope::from(s)
    }

    #[test]
    fn content_change_simple_insert_at_start() {
        let rope = make_rope("hello");
        // Insert "abc" at position 0 (no deletion)
        let delta = Delta::simple_edit(Interval::new(0, 0), XiRope::from("abc"), 5);
        let result = get_document_content_change(&rope, &delta);
        let change = result.expect("should be Some for simple insert");
        let range = change.range.expect("should have a range");
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 0);
        assert_eq!(range.end.line, 0);
        assert_eq!(range.end.character, 0);
        assert_eq!(change.text, "abc");
    }

    #[test]
    fn content_change_simple_insert_mid_text() {
        let rope = make_rope("hello world");
        // Insert "XY" at position 5 (between "hello" and " world")
        let delta = Delta::simple_edit(Interval::new(5, 5), XiRope::from("XY"), 11);
        let result = get_document_content_change(&rope, &delta);
        let change = result.expect("should be Some");
        let range = change.range.expect("range");
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 5);
        assert_eq!(range.end.character, 5);
        assert_eq!(change.text, "XY");
    }

    #[test]
    fn content_change_simple_insert_multiline() {
        let rope = make_rope("line1\nline2\nline3");
        // Insert at start of line2 (offset 6)
        let delta = Delta::simple_edit(Interval::new(6, 6), XiRope::from("NEW"), 17);
        let result = get_document_content_change(&rope, &delta);
        let change = result.expect("should be Some");
        let range = change.range.expect("range");
        assert_eq!(range.start.line, 1);
        assert_eq!(range.start.character, 0);
        assert_eq!(change.text, "NEW");
    }

    #[test]
    fn content_change_simple_delete() {
        let rope = make_rope("hello world");
        // Delete characters 5..11 (" world")
        let delta = Delta::simple_edit(Interval::new(5, 11), XiRope::from(""), 11);
        let result = get_document_content_change(&rope, &delta);
        let change = result.expect("should be Some for delete");
        let range = change.range.expect("range");
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 5);
        assert_eq!(range.end.line, 0);
        assert_eq!(range.end.character, 11);
        assert_eq!(change.text, "");
    }

    #[test]
    fn content_change_delete_across_lines() {
        let rope = make_rope("line1\nline2\nline3");
        // Delete from offset 3 ("e1\nline2\n") to offset 12
        let delta = Delta::simple_edit(Interval::new(3, 12), XiRope::from(""), 17);
        let result = get_document_content_change(&rope, &delta);
        let change = result.expect("should be Some");
        let range = change.range.expect("range");
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 3);
        assert_eq!(range.end.line, 2);
        assert_eq!(range.end.character, 0);
        assert_eq!(change.text, "");
    }

    #[test]
    fn content_change_replace_returns_none() {
        // A replace (delete + insert different text at same position) is complex
        // and should return None
        let rope = make_rope("hello");
        let delta =
            Delta::simple_edit(Interval::new(1, 3), XiRope::from("REPLACED"), 5);
        let result = get_document_content_change(&rope, &delta);
        // A replace that is not a simple insert or simple delete returns None
        // Note: simple_edit with non-empty interval and non-empty replacement
        // creates a replace delta, which is neither simple insert nor simple delete
        assert!(result.is_none());
    }

    #[test]
    fn content_change_empty_rope_insert() {
        let rope = make_rope("");
        let delta =
            Delta::simple_edit(Interval::new(0, 0), XiRope::from("hello"), 0);
        let result = get_document_content_change(&rope, &delta);
        let change = result.expect("should be Some");
        assert_eq!(change.text, "hello");
        let range = change.range.expect("range");
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 0);
    }

    // --- Buffer construction and methods ---

    #[test]
    fn buffer_new_from_existing_file() {
        let dir = std::env::temp_dir().join("lapce_test_buffer_new");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.rs");
        std::fs::write(&path, "fn main() {}").unwrap();

        let buf = Buffer::new(BufferId::next(), path.clone());
        assert_eq!(buf.rope.to_string(), "fn main() {}");
        assert_eq!(buf.language_id, "rust");
        assert!(!buf.read_only);
        assert_eq!(buf.rev, 1); // non-empty file starts at rev 1
        assert!(buf.mod_time.is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn buffer_new_nonexistent_file() {
        let path = PathBuf::from("/tmp/lapce_test_nonexistent_file_buffer.rs");
        let _ = std::fs::remove_file(&path); // ensure it doesn't exist
        let buf = Buffer::new(BufferId::next(), path);
        assert_eq!(buf.rope.to_string(), "");
        assert!(!buf.read_only); // NotFound treated as new file
        assert_eq!(buf.rev, 0); // empty file starts at rev 0
    }

    #[test]
    fn buffer_new_empty_file() {
        let dir = std::env::temp_dir().join("lapce_test_buffer_empty");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("empty.txt");
        std::fs::write(&path, "").unwrap();

        let buf = Buffer::new(BufferId::next(), path);
        assert_eq!(buf.rope.to_string(), "");
        assert_eq!(buf.rev, 0); // empty file starts at rev 0

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn buffer_offset_to_position_ascii() {
        let buf = Buffer {
            language_id: "rust",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("hello\nworld"),
            path: PathBuf::from("test.rs"),
            rev: 1,
            mod_time: None,
        };
        // "hello" is line 0, "world" is line 1
        let pos = buf.offset_to_position(0);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 0);

        let pos = buf.offset_to_position(5); // the '\n'
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 5);

        let pos = buf.offset_to_position(6); // 'w' in "world"
        assert_eq!(pos.line, 1);
        assert_eq!(pos.character, 0);

        let pos = buf.offset_to_position(11); // end of "world"
        assert_eq!(pos.line, 1);
        assert_eq!(pos.character, 5);
    }

    #[test]
    fn buffer_offset_to_position_multibyte() {
        // "café" has 5 bytes (é is 2 bytes), UTF-16 length is 4
        let buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("café"),
            path: PathBuf::from("test.txt"),
            rev: 1,
            mod_time: None,
        };
        // offset 0 → (0, 0)
        let pos = buf.offset_to_position(0);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 0);

        // offset 5 (end) → character 4 in UTF-16
        let pos = buf.offset_to_position(5);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 4);
    }

    #[test]
    fn buffer_offset_to_position_emoji() {
        // "😀" is 4 bytes in UTF-8, but 2 code units in UTF-16 (surrogate pair)
        let buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("a😀b"),
            path: PathBuf::from("test.txt"),
            rev: 1,
            mod_time: None,
        };
        // 'a' at offset 0
        let pos = buf.offset_to_position(0);
        assert_eq!(pos.character, 0);

        // '😀' starts at offset 1 → UTF-16 character 1
        let pos = buf.offset_to_position(1);
        assert_eq!(pos.character, 1);

        // 'b' starts at offset 5 → UTF-16 character 3 (1 for 'a' + 2 for 😀)
        let pos = buf.offset_to_position(5);
        assert_eq!(pos.character, 3);
    }

    #[test]
    fn buffer_update_correct_rev() {
        let mut buf = Buffer {
            language_id: "rust",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("hello"),
            path: PathBuf::from("test.rs"),
            rev: 1,
            mod_time: None,
        };
        // Insert " world" at end (rev must be current + 1 = 2)
        let delta =
            Delta::simple_edit(Interval::new(5, 5), XiRope::from(" world"), 5);
        let result = buf.update(&delta, 2);
        assert!(result.is_some());
        assert_eq!(buf.rev, 2);
        assert_eq!(buf.rope.to_string(), "hello world");
    }

    #[test]
    fn buffer_update_wrong_rev_returns_none() {
        let mut buf = Buffer {
            language_id: "rust",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("hello"),
            path: PathBuf::from("test.rs"),
            rev: 1,
            mod_time: None,
        };
        // Send rev=5 instead of expected rev=2
        let delta =
            Delta::simple_edit(Interval::new(5, 5), XiRope::from(" world"), 5);
        let result = buf.update(&delta, 5);
        assert!(result.is_none());
        assert_eq!(buf.rev, 1); // rev unchanged
        assert_eq!(buf.rope.to_string(), "hello"); // rope unchanged
    }

    #[test]
    fn buffer_update_sequential() {
        let mut buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("ab"),
            path: PathBuf::from("test.txt"),
            rev: 1,
            mod_time: None,
        };
        // Insert "X" at position 1 (between 'a' and 'b'), rev 2
        let delta = Delta::simple_edit(Interval::new(1, 1), XiRope::from("X"), 2);
        let r1 = buf.update(&delta, 2);
        assert!(r1.is_some());
        assert_eq!(buf.rope.to_string(), "aXb");
        assert_eq!(buf.rev, 2);

        // Delete 'X' (offset 1..2), rev 3
        let delta = Delta::simple_edit(Interval::new(1, 2), XiRope::from(""), 3);
        let r2 = buf.update(&delta, 3);
        assert!(r2.is_some());
        assert_eq!(buf.rope.to_string(), "ab");
        assert_eq!(buf.rev, 3);
    }

    #[test]
    fn buffer_update_complex_delta_falls_back_to_full_text() {
        let mut buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("hello"),
            path: PathBuf::from("test.txt"),
            rev: 1,
            mod_time: None,
        };
        // A replace is not simple insert or delete, so content_change is None
        // and the update falls back to full document text
        let delta =
            Delta::simple_edit(Interval::new(1, 3), XiRope::from("REPLACED"), 5);
        let result = buf.update(&delta, 2);
        let change = result.expect("update should succeed");
        // When get_document_content_change returns None, range is None (full doc)
        assert!(change.range.is_none());
        assert_eq!(change.text, "hREPLACEDlo");
    }

    #[test]
    fn buffer_len_and_is_empty() {
        let buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("abc"),
            path: PathBuf::from("test.txt"),
            rev: 1,
            mod_time: None,
        };
        assert_eq!(buf.len(), 3);
        assert!(!buf.is_empty());

        let empty_buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from(""),
            path: PathBuf::from("test.txt"),
            rev: 0,
            mod_time: None,
        };
        assert_eq!(empty_buf.len(), 0);
        assert!(empty_buf.is_empty());
    }

    #[test]
    fn buffer_get_document() {
        let buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("line1\nline2"),
            path: PathBuf::from("test.txt"),
            rev: 1,
            mod_time: None,
        };
        assert_eq!(buf.get_document(), "line1\nline2");
    }

    #[test]
    fn buffer_offset_of_line_and_line_of_offset() {
        let buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("abc\ndef\nghi"),
            path: PathBuf::from("test.txt"),
            rev: 1,
            mod_time: None,
        };
        assert_eq!(buf.offset_of_line(0), 0);
        assert_eq!(buf.offset_of_line(1), 4);
        assert_eq!(buf.offset_of_line(2), 8);
        assert_eq!(buf.line_of_offset(0), 0);
        assert_eq!(buf.line_of_offset(4), 1);
        assert_eq!(buf.line_of_offset(8), 2);
    }

    #[test]
    fn buffer_offset_to_line_col() {
        let buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("abc\ndef"),
            path: PathBuf::from("test.txt"),
            rev: 1,
            mod_time: None,
        };
        assert_eq!(buf.offset_to_line_col(0), (0, 0));
        assert_eq!(buf.offset_to_line_col(2), (0, 2));
        assert_eq!(buf.offset_to_line_col(4), (1, 0));
        assert_eq!(buf.offset_to_line_col(6), (1, 2));
    }

    // --- save tests ---

    #[test]
    fn buffer_save_basic() {
        let dir = std::env::temp_dir().join("lapce_test_buffer_save");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("save_test.txt");
        std::fs::write(&path, "original").unwrap();

        let mut buf = Buffer::new(BufferId::next(), path.clone());
        // Modify the buffer
        buf.rope = XiRope::from("modified content");
        let result = buf.save(buf.rev, false);
        assert!(result.is_ok());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved, "modified content");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn buffer_save_read_only_rejected() {
        let mut buf = Buffer {
            language_id: "",
            read_only: true,
            id: BufferId::next(),
            rope: XiRope::from("content"),
            path: PathBuf::from("/tmp/lapce_test_readonly.txt"),
            rev: 1,
            mod_time: None,
        };
        let result = buf.save(1, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("read only"));
    }

    #[test]
    fn buffer_save_wrong_rev_rejected() {
        let dir = std::env::temp_dir().join("lapce_test_buffer_save_rev");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("rev_test.txt");
        std::fs::write(&path, "content").unwrap();

        let mut buf = Buffer::new(BufferId::next(), path);
        let result = buf.save(999, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("rev"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn buffer_save_creates_parents() {
        let dir = std::env::temp_dir().join("lapce_test_save_parents");
        let _ = std::fs::remove_dir_all(&dir); // clean up from previous runs
        let nested = dir.join("a").join("b").join("c");
        let path = nested.join("file.txt");

        let mut buf = Buffer {
            language_id: "",
            read_only: false,
            id: BufferId::next(),
            rope: XiRope::from("new file"),
            path: path.clone(),
            rev: 1,
            mod_time: None,
        };
        let result = buf.save(1, true);
        assert!(result.is_ok());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved, "new file");

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- get_mod_time ---

    #[test]
    fn get_mod_time_existing_file() {
        let dir = std::env::temp_dir().join("lapce_test_mod_time");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("modtime_test.txt");
        std::fs::write(&path, "content").unwrap();

        let mt = get_mod_time(&path);
        assert!(mt.is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn get_mod_time_nonexistent() {
        let mt = get_mod_time("/tmp/lapce_definitely_not_a_file_12345");
        assert!(mt.is_none());
    }
}
