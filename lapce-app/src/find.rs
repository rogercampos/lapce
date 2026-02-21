use std::cmp::{max, min};

use floem::{
    prelude::SignalTrack,
    reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
};
use lapce_core::{
    selection::{SelRegion, Selection},
    word::WordCursor,
};
use lapce_xi_rope::{
    Cursor, Interval, Rope,
    find::{CaseMatching, find, is_multiline_regex},
};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

/// Safety limit on compiled regex size to prevent denial-of-service from
/// pathological patterns (e.g., deeply nested quantifiers). 1MB is generous
/// for typical search patterns.
const REGEX_SIZE_LIMIT: usize = 1000000;

/// Indicates what changed in the find state.
#[derive(PartialEq, Debug, Clone)]
pub enum FindProgress {
    /// Incremental find is done/not running.
    Ready,

    /// The find process just started.
    Started,

    /// Incremental find is in progress. Keeps tracked of already searched range.
    InProgress(Selection),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FindStatus {
    /// Identifier for the current search query.
    id: usize,

    /// The current search query.
    chars: Option<String>,

    /// Whether the active search is case matching.
    case_sensitive: Option<bool>,

    /// Whether the search query is considered as regular expression.
    is_regex: Option<bool>,

    /// Query only matches whole words.
    whole_words: Option<bool>,

    /// Total number of matches.
    matches: usize,

    /// Line numbers which have find results.
    lines: Vec<usize>,
}

#[derive(Clone)]
pub struct FindSearchString {
    pub content: String,
    pub regex: Option<Regex>,
}

#[derive(Clone)]
pub struct Find {
    pub rev: RwSignal<u64>,
    /// If the find is shown
    pub visual: RwSignal<bool>,
    /// The currently active search string.
    pub search_string: RwSignal<Option<FindSearchString>>,
    /// The case matching setting for the currently active search.
    pub case_matching: RwSignal<CaseMatching>,
    /// Query matches only whole words.
    pub whole_words: RwSignal<bool>,
    /// The search query should be considered as regular expression.
    pub is_regex: RwSignal<bool>,
    /// replace editor is shown
    pub replace_active: RwSignal<bool>,
    /// replace editor is focused
    pub replace_focus: RwSignal<bool>,
    /// Triggered by changes in the search string
    pub triggered_by_changes: RwSignal<bool>,
}

impl Find {
    pub fn new(cx: Scope) -> Self {
        let find = Self {
            rev: cx.create_rw_signal(0),
            visual: cx.create_rw_signal(false),
            search_string: cx.create_rw_signal(None),
            case_matching: cx.create_rw_signal(CaseMatching::CaseInsensitive),
            whole_words: cx.create_rw_signal(false),
            is_regex: cx.create_rw_signal(false),
            replace_active: cx.create_rw_signal(false),
            replace_focus: cx.create_rw_signal(false),
            triggered_by_changes: cx.create_rw_signal(false),
        };

        {
            let find = find.clone();
            cx.create_effect(move |_| {
                find.is_regex.with(|_| ());
                let s = find.search_string.with_untracked(|s| {
                    if let Some(s) = s.as_ref() {
                        s.content.clone()
                    } else {
                        "".to_string()
                    }
                });
                if !s.is_empty() {
                    find.set_find(&s);
                }
            });
        }

        {
            let find = find.clone();
            cx.create_effect(move |_| {
                find.search_string.track();
                find.case_matching.track();
                find.whole_words.track();
                find.rev.update(|rev| {
                    *rev += 1;
                });
            });
        }

        find
    }

    /// Returns `true` if case sensitive, otherwise `false`
    pub fn case_sensitive(&self, tracked: bool) -> bool {
        match if tracked {
            self.case_matching.get()
        } else {
            self.case_matching.get_untracked()
        } {
            CaseMatching::Exact => true,
            CaseMatching::CaseInsensitive => false,
        }
    }

    /// Sets find case sensitivity.
    pub fn set_case_sensitive(&self, case_sensitive: bool) {
        if self.case_sensitive(false) == case_sensitive {
            return;
        }

        let case_matching = if case_sensitive {
            CaseMatching::Exact
        } else {
            CaseMatching::CaseInsensitive
        };
        self.case_matching.set(case_matching);
    }

    pub fn set_find(&self, search_string: &str) {
        if search_string.is_empty() {
            self.search_string.set(None);
            return;
        }

        let is_regex = self.is_regex.get_untracked();

        let search_string_unchanged = self.search_string.with_untracked(|search| {
            if let Some(s) = search {
                s.content == search_string && s.regex.is_some() == is_regex
            } else {
                false
            }
        });

        if search_string_unchanged {
            return;
        }

        // create regex from untrusted input
        let regex = match is_regex {
            false => None,
            true => RegexBuilder::new(search_string)
                .size_limit(REGEX_SIZE_LIMIT)
                .case_insensitive(!self.case_sensitive(false))
                .build()
                .ok(),
        };
        self.triggered_by_changes.set(true);
        self.search_string.set(Some(FindSearchString {
            content: search_string.to_string(),
            regex,
        }));
    }

    /// Find the next (or previous if reverse=true) occurrence of the search pattern
    /// relative to the given offset. When wrap=true, wraps around the document boundaries.
    /// Returns the (start, end) byte offsets of the match, or None if not found.
    /// For reverse search, we must collect all matches before the offset and take the last,
    /// because the underlying find() only searches forward.
    pub fn next(
        &self,
        text: &Rope,
        offset: usize,
        reverse: bool,
        wrap: bool,
    ) -> Option<(usize, usize)> {
        if !self.visual.get_untracked() {
            self.visual.set(true);
        }
        let case_matching = self.case_matching.get_untracked();
        let whole_words = self.whole_words.get_untracked();
        self.search_string.with_untracked(
            |search_string| -> Option<(usize, usize)> {
                let search_string = search_string.as_ref()?;
                if !reverse {
                    let mut raw_lines = text.lines_raw(offset..text.len());
                    let mut find_cursor = Cursor::new(text, offset);
                    while let Some(start) = find(
                        &mut find_cursor,
                        &mut raw_lines,
                        case_matching,
                        &search_string.content,
                        search_string.regex.as_ref(),
                    ) {
                        let end = find_cursor.pos();

                        if whole_words
                            && !Self::is_matching_whole_words(text, start, end)
                        {
                            raw_lines =
                                text.lines_raw(find_cursor.pos()..text.len());
                            continue;
                        }
                        raw_lines = text.lines_raw(find_cursor.pos()..text.len());

                        if start > offset {
                            return Some((start, end));
                        }
                    }
                    if wrap {
                        let mut raw_lines = text.lines_raw(0..offset);
                        let mut find_cursor = Cursor::new(text, 0);
                        while let Some(start) = find(
                            &mut find_cursor,
                            &mut raw_lines,
                            case_matching,
                            &search_string.content,
                            search_string.regex.as_ref(),
                        ) {
                            let end = find_cursor.pos();

                            if whole_words
                                && !Self::is_matching_whole_words(text, start, end)
                            {
                                raw_lines =
                                    text.lines_raw(find_cursor.pos()..offset);
                                continue;
                            }
                            return Some((start, end));
                        }
                    }
                } else {
                    let mut raw_lines = text.lines_raw(0..offset);
                    let mut find_cursor = Cursor::new(text, 0);
                    let mut last_match = None;
                    while let Some(start) = find(
                        &mut find_cursor,
                        &mut raw_lines,
                        case_matching,
                        &search_string.content,
                        search_string.regex.as_ref(),
                    ) {
                        let end = find_cursor.pos();
                        raw_lines = text.lines_raw(find_cursor.pos()..offset);
                        if whole_words
                            && !Self::is_matching_whole_words(text, start, end)
                        {
                            continue;
                        }
                        if start < offset {
                            last_match = Some((start, end));
                        }
                    }
                    if last_match.is_some() {
                        return last_match;
                    }
                    if wrap {
                        let mut raw_lines = text.lines_raw(offset..text.len());
                        let mut find_cursor = Cursor::new(text, offset);
                        let mut last_match = None;
                        while let Some(start) = find(
                            &mut find_cursor,
                            &mut raw_lines,
                            case_matching,
                            &search_string.content,
                            search_string.regex.as_ref(),
                        ) {
                            let end = find_cursor.pos();

                            if whole_words
                                && !Self::is_matching_whole_words(text, start, end)
                            {
                                raw_lines =
                                    text.lines_raw(find_cursor.pos()..text.len());
                                continue;
                            }
                            raw_lines =
                                text.lines_raw(find_cursor.pos()..text.len());

                            if start > offset {
                                last_match = Some((start, end));
                            }
                        }
                        if last_match.is_some() {
                            return last_match;
                        }
                    }
                }
                None
            },
        )
    }

    /// Checks if the start and end of a match is matching whole words.
    fn is_matching_whole_words(text: &Rope, start: usize, end: usize) -> bool {
        if end == 0 || start >= text.len() {
            return false;
        }
        let mut word_end_cursor = WordCursor::new(text, end - 1);
        let mut word_start_cursor = WordCursor::new(text, start + 1);

        if word_start_cursor.prev_code_boundary() != start {
            return false;
        }

        if word_end_cursor.next_code_boundary() != end {
            return false;
        }

        true
    }

    /// Returns `true` if the search query is a multi-line regex.
    pub fn is_multiline_regex(&self) -> bool {
        self.search_string.with_untracked(|search| {
            if let Some(search) = search.as_ref() {
                search.regex.is_some() && is_multiline_regex(&search.content)
            } else {
                false
            }
        })
    }

    /// Search for all occurrences of the pattern within the [start, end] range and add
    /// them to `occurrences`. The "slop" expands the search range by 2x the search string
    /// length on each side -- this catches matches that span the region boundaries
    /// (e.g., a match that starts just before `start` and ends within the range).
    #[allow(clippy::too_many_arguments)]
    pub fn find(
        text: &Rope,
        search: &FindSearchString,
        start: usize,
        end: usize,
        case_matching: CaseMatching,
        whole_words: bool,
        include_slop: bool,
        occurrences: &mut Selection,
    ) {
        let search_string = &search.content;

        let slop = if include_slop {
            search.content.len() * 2
        } else {
            0
        };

        // expand region to be able to find occurrences around the region's edges
        let expanded_start = max(start, slop) - slop;
        let expanded_end = min(end.saturating_add(slop), text.len());
        let from = text
            .at_or_prev_codepoint_boundary(expanded_start)
            .unwrap_or(0);
        let to = text
            .at_or_next_codepoint_boundary(expanded_end)
            .unwrap_or_else(|| text.len());
        let mut to_cursor = Cursor::new(text, to);
        let _ = to_cursor.next_leaf();

        let sub_text = text.subseq(Interval::new(0, to_cursor.pos()));
        let mut find_cursor = Cursor::new(&sub_text, from);

        let mut raw_lines = text.lines_raw(from..to);

        while let Some(start) = find(
            &mut find_cursor,
            &mut raw_lines,
            case_matching,
            search_string,
            search.regex.as_ref(),
        ) {
            let end = find_cursor.pos();

            if whole_words && !Self::is_matching_whole_words(text, start, end) {
                raw_lines = text.lines_raw(find_cursor.pos()..to);
                continue;
            }

            let region = SelRegion::new(start, end, None);
            let (_, e) = occurrences.add_range_distinct(region);
            // in case of ambiguous search results (e.g. search "aba" in "ababa"),
            // the search result closer to the beginning of the file wins
            if e != end {
                // Skip the search result and keep the occurrence that is closer to
                // the beginning of the file. Re-align the cursor to the kept
                // occurrence
                find_cursor.set(e);
                raw_lines = text.lines_raw(find_cursor.pos()..to);
                continue;
            }

            // in case current cursor matches search result (for example query a* matches)
            // all cursor positions, then cursor needs to be increased so that search
            // continues at next position. Otherwise, search will result in overflow since
            // search will always repeat at current cursor position.
            if start == end {
                // determine whether end of text is reached and stop search or increase
                // cursor manually
                if end + 1 >= text.len() {
                    break;
                } else {
                    find_cursor.set(end + 1);
                }
            }

            // update line iterator so that line starts at current cursor position
            raw_lines = text.lines_raw(find_cursor.pos()..to);
        }
    }

    /// Execute the search on the provided text in the range provided by `start` and `end`.
    pub fn update_find(
        &self,
        text: &Rope,
        start: usize,
        end: usize,
        include_slop: bool,
        occurrences: &mut Selection,
    ) {
        if self.search_string.with_untracked(|search| search.is_none()) {
            return;
        }

        let search = self.search_string.get_untracked().unwrap();
        let search_string = &search.content;
        // extend the search by twice the string length (twice, because case matching may increase
        // the length of an occurrence)
        let slop = if include_slop {
            search.content.len() * 2
        } else {
            0
        };

        // expand region to be able to find occurrences around the region's edges
        let expanded_start = max(start, slop) - slop;
        let expanded_end = min(end.saturating_add(slop), text.len());
        let from = text
            .at_or_prev_codepoint_boundary(expanded_start)
            .unwrap_or(0);
        let to = text
            .at_or_next_codepoint_boundary(expanded_end)
            .unwrap_or_else(|| text.len());
        let mut to_cursor = Cursor::new(text, to);
        let _ = to_cursor.next_leaf();

        let sub_text = text.subseq(Interval::new(0, to_cursor.pos()));
        let mut find_cursor = Cursor::new(&sub_text, from);

        let mut raw_lines = text.lines_raw(from..to);

        let case_matching = self.case_matching.get_untracked();
        let whole_words = self.whole_words.get_untracked();
        while let Some(start) = find(
            &mut find_cursor,
            &mut raw_lines,
            case_matching,
            search_string,
            search.regex.as_ref(),
        ) {
            let end = find_cursor.pos();

            if whole_words && !Self::is_matching_whole_words(text, start, end) {
                raw_lines = text.lines_raw(find_cursor.pos()..to);
                continue;
            }

            let region = SelRegion::new(start, end, None);
            let (_, e) = occurrences.add_range_distinct(region);
            // in case of ambiguous search results (e.g. search "aba" in "ababa"),
            // the search result closer to the beginning of the file wins
            if e != end {
                // Skip the search result and keep the occurrence that is closer to
                // the beginning of the file. Re-align the cursor to the kept
                // occurrence
                find_cursor.set(e);
                raw_lines = text.lines_raw(find_cursor.pos()..to);
                continue;
            }

            // in case current cursor matches search result (for example query a* matches)
            // all cursor positions, then cursor needs to be increased so that search
            // continues at next position. Otherwise, search will result in overflow since
            // search will always repeat at current cursor position.
            if start == end {
                // determine whether end of text is reached and stop search or increase
                // cursor manually
                if end + 1 >= text.len() {
                    break;
                } else {
                    find_cursor.set(end + 1);
                }
            }

            // update line iterator so that line starts at current cursor position
            raw_lines = text.lines_raw(find_cursor.pos()..to);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(
        text: &str,
        pattern: &str,
        case_matching: CaseMatching,
        whole_words: bool,
        is_regex: bool,
    ) -> Vec<(usize, usize)> {
        let rope = Rope::from(text);
        let regex = if is_regex {
            RegexBuilder::new(pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .case_insensitive(matches!(
                    case_matching,
                    CaseMatching::CaseInsensitive
                ))
                .build()
                .ok()
        } else {
            None
        };
        let search = FindSearchString {
            content: pattern.to_string(),
            regex,
        };
        let mut occurrences = Selection::new();
        Find::find(
            &rope,
            &search,
            0,
            rope.len(),
            case_matching,
            whole_words,
            false,
            &mut occurrences,
        );
        occurrences
            .regions()
            .iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    // ----- Find::find() static method -----

    #[test]
    fn find_simple_literal() {
        let results =
            search("hello world", "world", CaseMatching::Exact, false, false);
        assert_eq!(results, vec![(6, 11)]);
    }

    #[test]
    fn find_multiple_occurrences() {
        let results = search("abcabc", "abc", CaseMatching::Exact, false, false);
        assert_eq!(results, vec![(0, 3), (3, 6)]);
    }

    #[test]
    fn find_no_match() {
        let results =
            search("hello world", "xyz", CaseMatching::Exact, false, false);
        assert!(results.is_empty());
    }

    #[test]
    fn find_case_insensitive() {
        let results = search(
            "Hello HELLO hello",
            "hello",
            CaseMatching::CaseInsensitive,
            false,
            false,
        );
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn find_case_sensitive() {
        let results = search(
            "Hello HELLO hello",
            "hello",
            CaseMatching::Exact,
            false,
            false,
        );
        assert_eq!(results, vec![(12, 17)]);
    }

    #[test]
    fn find_whole_words_only() {
        let results = search(
            "cat concatenate caterpillar cat",
            "cat",
            CaseMatching::Exact,
            true,
            false,
        );
        // Only standalone "cat" matches, not "concatenate" or "caterpillar"
        assert_eq!(results, vec![(0, 3), (28, 31)]);
    }

    #[test]
    fn find_regex_pattern() {
        let results = search(
            "foo123 bar456 baz",
            r"\d+",
            CaseMatching::Exact,
            false,
            true,
        );
        assert_eq!(results, vec![(3, 6), (10, 13)]);
    }

    #[test]
    fn find_regex_case_insensitive() {
        let results = search(
            "Foo foo FOO",
            "foo",
            CaseMatching::CaseInsensitive,
            false,
            true,
        );
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn find_empty_text() {
        let results = search("", "hello", CaseMatching::Exact, false, false);
        assert!(results.is_empty());
    }

    #[test]
    fn find_at_boundaries() {
        let results = search("abc", "abc", CaseMatching::Exact, false, false);
        assert_eq!(results, vec![(0, 3)]);
    }

    #[test]
    fn find_multiline() {
        let results = search(
            "line1\nline2\nline3",
            "line",
            CaseMatching::Exact,
            false,
            false,
        );
        assert_eq!(results, vec![(0, 4), (6, 10), (12, 16)]);
    }

    #[test]
    fn find_with_slop_includes_boundary_matches() {
        let rope = Rope::from("hello world hello");
        let search = FindSearchString {
            content: "hello".to_string(),
            regex: None,
        };
        let mut occurrences = Selection::new();
        // Search a middle range with slop enabled
        Find::find(
            &rope,
            &search,
            5,
            12,
            CaseMatching::Exact,
            false,
            true, // include_slop
            &mut occurrences,
        );
        let results: Vec<(usize, usize)> = occurrences
            .regions()
            .iter()
            .map(|r| (r.start, r.end))
            .collect();
        // With slop, the search range expands to catch matches near boundaries
        assert!(results.contains(&(0, 5)));
        assert!(results.contains(&(12, 17)));
    }

    #[test]
    fn find_overlapping_pattern_first_wins() {
        // "aba" in "ababa" — ambiguous match, first occurrence wins
        let results = search("ababa", "aba", CaseMatching::Exact, false, false);
        assert_eq!(results, vec![(0, 3)]);
    }

    #[test]
    fn find_unicode_text() {
        let results = search(
            "café résumé café",
            "café",
            CaseMatching::Exact,
            false,
            false,
        );
        assert_eq!(results.len(), 2);
    }

    // ----- is_matching_whole_words -----

    #[test]
    fn whole_words_standalone_word() {
        let rope = Rope::from("hello world");
        assert!(Find::is_matching_whole_words(&rope, 0, 5)); // "hello"
        assert!(Find::is_matching_whole_words(&rope, 6, 11)); // "world"
    }

    #[test]
    fn whole_words_not_at_word_boundary() {
        let rope = Rope::from("caterpillar");
        assert!(!Find::is_matching_whole_words(&rope, 0, 3)); // "cat" is not a whole word
    }

    #[test]
    fn whole_words_with_punctuation_boundary() {
        let rope = Rope::from("hello,world");
        assert!(Find::is_matching_whole_words(&rope, 0, 5)); // "hello" with comma after
        assert!(Find::is_matching_whole_words(&rope, 6, 11)); // "world" with comma before
    }

    // ----- Find::find() with regex edge cases -----

    #[test]
    fn find_regex_zero_length_match_advances() {
        // a* matches empty strings — the search should not loop infinitely
        let results = search("abc", "a*", CaseMatching::Exact, false, true);
        // Should get matches without hanging
        assert!(!results.is_empty());
    }

    #[test]
    fn find_whole_words_with_regex() {
        let results = search(
            "cat concatenate cat",
            "cat",
            CaseMatching::Exact,
            true,
            true, // regex mode
        );
        // Only standalone "cat" matches
        assert_eq!(results, vec![(0, 3), (16, 19)]);
    }

    #[test]
    fn find_single_char_pattern() {
        let results = search("aaa", "a", CaseMatching::Exact, false, false);
        assert_eq!(results, vec![(0, 1), (1, 2), (2, 3)]);
    }

    #[test]
    fn find_whole_words_at_text_boundaries() {
        // Word at the very start of text
        let results =
            search("hello world", "hello", CaseMatching::Exact, true, false);
        assert_eq!(results, vec![(0, 5)]);

        // Word at the very end of text
        let results =
            search("hello world", "world", CaseMatching::Exact, true, false);
        assert_eq!(results, vec![(6, 11)]);
    }

    #[test]
    fn find_whole_words_rejects_partial() {
        // Should NOT match "hell" inside "hello"
        let results =
            search("hello world", "hell", CaseMatching::Exact, true, false);
        assert!(results.is_empty());

        // Should NOT match "orld" inside "world"
        let results =
            search("hello world", "orld", CaseMatching::Exact, true, false);
        assert!(results.is_empty());
    }

    #[test]
    fn find_whole_words_single_char_text() {
        let results = search("a", "a", CaseMatching::Exact, true, false);
        assert_eq!(results, vec![(0, 1)]);
    }

    #[test]
    fn find_whole_words_empty_text() {
        let results = search("", "hello", CaseMatching::Exact, true, false);
        assert!(results.is_empty());
    }

    #[test]
    fn find_without_slop_exact_range() {
        let rope = Rope::from("abcdefghij");
        let search = FindSearchString {
            content: "abc".to_string(),
            regex: None,
        };
        let mut occurrences = Selection::new();
        // Search only in range 3..10, should not find "abc" at offset 0
        Find::find(
            &rope,
            &search,
            3,
            10,
            CaseMatching::Exact,
            false,
            false,
            &mut occurrences,
        );
        let results: Vec<(usize, usize)> = occurrences
            .regions()
            .iter()
            .map(|r| (r.start, r.end))
            .collect();
        assert!(results.is_empty());
    }
}

/// Per-document find results. Each open document has its own FindResult which stores
/// the computed match occurrences. The `progress` signal tracks incremental search state
/// so the editor can show partial results while a large document is still being searched.
/// The signals here mirror the Find settings so each document can detect when it needs
/// to re-run the search (e.g., find settings changed but the document hasn't been re-scanned yet).
#[derive(Clone)]
pub struct FindResult {
    pub find_rev: RwSignal<u64>,
    pub progress: RwSignal<FindProgress>,
    pub occurrences: RwSignal<Selection>,
    pub search_string: RwSignal<Option<FindSearchString>>,
    pub case_matching: RwSignal<CaseMatching>,
    pub whole_words: RwSignal<bool>,
    pub is_regex: RwSignal<bool>,
}

impl FindResult {
    pub fn new(cx: Scope) -> Self {
        Self {
            find_rev: cx.create_rw_signal(0),
            progress: cx.create_rw_signal(FindProgress::Started),
            occurrences: cx.create_rw_signal(Selection::new()),
            search_string: cx.create_rw_signal(None),
            case_matching: cx.create_rw_signal(CaseMatching::Exact),
            whole_words: cx.create_rw_signal(false),
            is_regex: cx.create_rw_signal(false),
        }
    }

    pub fn reset(&self) {
        self.progress.set(FindProgress::Started);
    }
}
