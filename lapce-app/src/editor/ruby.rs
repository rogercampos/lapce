//! Ruby-specific helpers used by the editor's go-to-definition pipeline.
//!
//! Ruby's LSP (ruby-lsp / Sorbet) emits slightly awkward results we fix up
//! on the client side: sigil-prefixed identifiers (`@var`, `@@class_var`,
//! `$global`) don't round-trip through the normal `prev_code_boundary`, and
//! definition responses often include both a gem source location and its
//! stdlib `.rbi` shadow, which would show the picker twice for the same
//! symbol. The functions here are `pub(crate)` because the go-to-definition
//! logic lives in `editor/ops_lsp.rs` and the Cmd+hover link logic lives in
//! `editor/ops_pointer.rs`.

use std::collections::HashSet;

use lapce_core::buffer::{Buffer, rope_text::RopeText};
use lsp_types::Location;

/// Extend the word-start boundary backwards to include Ruby sigils so that
/// Cmd+click and go-to-definition land on the full identifier. Supports
/// `@ivar`, `@@cvar`, and `$global`.
pub(crate) fn ruby_word_start(buffer: &Buffer, word_start: usize) -> usize {
    if word_start == 0 {
        return word_start;
    }
    let prev = buffer.slice_to_cow(word_start - 1..word_start);
    if prev == "@" {
        if word_start >= 2
            && buffer.slice_to_cow(word_start - 2..word_start - 1) == "@"
        {
            word_start - 2 // @@class_var
        } else {
            word_start - 1 // @instance_var
        }
    } else if prev == "$" {
        word_start - 1 // $global_var
    } else {
        word_start
    }
}

/// Returns true if the URI points to a Ruby type definition file (.rbs or .rbi).
pub(crate) fn is_ruby_type_file(uri: &lsp_types::Url) -> bool {
    let path = uri.path();
    path.ends_with(".rbs") || path.ends_with(".rbi")
}

/// Remove locations pointing to Ruby type definition files (.rbs, .rbi).
pub(crate) fn ruby_filter_type_files(locations: &mut Vec<Location>) {
    locations.retain(|l| !is_ruby_type_file(&l.uri));
}

/// When a symbol is defined in both a bundled gem and the Ruby stdlib shadow,
/// drop the stdlib duplicate.
///
/// Detection: a stdlib path contains `/lib/ruby/<ver>/<relpath>` without `/gems/`.
/// A gem path contains `/gems/<name>/lib/<relpath>`. If `<relpath>` matches, the
/// stdlib entry is redundant.
pub(crate) fn dedup_ruby_stdlib_gems(locations: &mut Vec<Location>) {
    if locations.len() < 2 {
        return;
    }

    // Collect relative paths from gem locations: /gems/<gem-name-ver>/lib/<relpath>
    let gem_rel_paths: HashSet<String> = locations
        .iter()
        .filter_map(|l| {
            let path = l.uri.path();
            // Find the last /gems/<gem-name-ver>/lib/ and extract the relative path
            let mut search_from = 0;
            let mut result = None;
            while let Some(idx) = path[search_from..].find("/gems/") {
                let abs_idx = search_from + idx;
                let after = &path[abs_idx + "/gems/".len()..];
                if let Some(slash) = after.find('/') {
                    let rest = &after[slash + 1..];
                    if let Some(rel) = rest.strip_prefix("lib/") {
                        if !rel.is_empty() {
                            result = Some(rel.to_string());
                        }
                    }
                }
                search_from = abs_idx + 1;
            }
            result
        })
        .collect();

    if gem_rel_paths.is_empty() {
        return;
    }

    // Remove stdlib locations whose relative path matches a gem entry.
    // Stdlib pattern: /lib/ruby/<ver>/<relpath> where the segment is NOT "gems/"
    locations.retain(|l| {
        let path = l.uri.path();
        if let Some(idx) = path.find("/lib/ruby/") {
            let after = &path[idx + "/lib/ruby/".len()..];
            if !after.starts_with("gems/") {
                if let Some(slash) = after.find('/') {
                    let rel_path = &after[slash + 1..];
                    if !rel_path.is_empty() && gem_rel_paths.contains(rel_path) {
                        return false; // Drop this stdlib duplicate
                    }
                }
            }
        }
        true
    });
}
