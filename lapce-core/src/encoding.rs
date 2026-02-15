/// Convert a utf8 byte offset into a utf16 code-unit offset.
/// This is needed because LSP uses utf16 offsets (matching JavaScript's
/// string encoding) while Rust strings are utf8. For pure ASCII text
/// the offsets are identical, but multi-byte characters cause divergence.
///
/// Handles edge cases: offset inside a multi-byte character returns the
/// position of that character; offset past the end returns the utf16 length.
pub fn offset_utf8_to_utf16(
    char_indices: impl Iterator<Item = (usize, char)>,
    offset: usize,
) -> usize {
    if offset == 0 {
        return 0;
    }

    let mut utf16_offset = 0;
    let mut last_ich = None;
    for (utf8_offset, ch) in char_indices {
        last_ich = Some((utf8_offset, ch));

        match utf8_offset.cmp(&offset) {
            std::cmp::Ordering::Less => {}
            // We found the right offset
            std::cmp::Ordering::Equal => {
                return utf16_offset;
            }
            // Implies that the offset was inside of a character
            std::cmp::Ordering::Greater => return utf16_offset,
        }

        utf16_offset += ch.len_utf16();
    }

    // TODO: We could use TrustedLen when that is stabilized and it is impl'd on
    // the iterators we use

    // We did not find the offset. This means that it is either at the end
    // or past the end.
    let text_len = last_ich.map(|(i, c)| i + c.len_utf8());
    if text_len == Some(offset) {
        // Since the utf16 offset was being incremented each time, by now it is equivalent to the length
        // but in utf16 characters
        return utf16_offset;
    }

    utf16_offset
}

pub fn offset_utf8_to_utf16_str(text: &str, offset: usize) -> usize {
    offset_utf8_to_utf16(text.char_indices(), offset)
}

/// Convert a utf16 offset into a utf8 offset, if possible  
/// `char_indices` is an iterator over utf8 offsets and the characters
/// It is cloneable so that it can be iterated multiple times. Though it should be cheaply cloneable.
pub fn offset_utf16_to_utf8(
    char_indices: impl Iterator<Item = (usize, char)>,
    offset: usize,
) -> usize {
    if offset == 0 {
        return 0;
    }

    // We accumulate the utf16 char lens until we find the utf8 offset that matches it
    // or, we find out that it went into the middle of sometext
    // We also keep track of the last offset and char in order to calculate the length of the text
    // if we the index was at the end of the string
    let mut utf16_offset = 0;
    let mut last_ich = None;
    for (utf8_offset, ch) in char_indices {
        last_ich = Some((utf8_offset, ch));

        let ch_utf16_len = ch.len_utf16();

        match utf16_offset.cmp(&offset) {
            std::cmp::Ordering::Less => {}
            // We found the right offset
            std::cmp::Ordering::Equal => {
                return utf8_offset;
            }
            // This implies that the offset was in the middle of a character as we skipped over it
            std::cmp::Ordering::Greater => return utf8_offset,
        }

        utf16_offset += ch_utf16_len;
    }

    // We did not find the offset, this means that it was either at the end
    // or past the end
    // Since we've iterated over all the char indices, the utf16_offset is now the
    // utf16 length
    if let Some((last_utf8_offset, last_ch)) = last_ich {
        last_utf8_offset + last_ch.len_utf8()
    } else {
        0
    }
}

pub fn offset_utf16_to_utf8_str(text: &str, offset: usize) -> usize {
    offset_utf16_to_utf8(text.char_indices(), offset)
}

#[cfg(test)]
mod tests {
    // TODO: more tests with unicode characters

    use crate::encoding::{offset_utf8_to_utf16_str, offset_utf16_to_utf8_str};

    #[test]
    fn utf8_to_utf16() {
        let text = "hello world";

        assert_eq!(offset_utf8_to_utf16_str(text, 0), 0);
        assert_eq!(offset_utf8_to_utf16_str("", 0), 0);

        assert_eq!(offset_utf8_to_utf16_str("", 1), 0);

        assert_eq!(offset_utf8_to_utf16_str("h", 0), 0);
        assert_eq!(offset_utf8_to_utf16_str("h", 1), 1);

        assert_eq!(offset_utf8_to_utf16_str(text, text.len()), text.len());

        assert_eq!(
            offset_utf8_to_utf16_str(text, text.len() - 1),
            text.len() - 1
        );

        assert_eq!(offset_utf8_to_utf16_str(text, text.len() + 1), text.len());

        assert_eq!(offset_utf8_to_utf16_str("×", 0), 0);
        assert_eq!(offset_utf8_to_utf16_str("×", 1), 1);
        assert_eq!(offset_utf8_to_utf16_str("×", 2), 1);
        assert_eq!(offset_utf8_to_utf16_str("a×", 0), 0);
        assert_eq!(offset_utf8_to_utf16_str("a×", 1), 1);
        assert_eq!(offset_utf8_to_utf16_str("a×", 2), 2);
        assert_eq!(offset_utf8_to_utf16_str("a×", 3), 2);
    }

    #[test]
    fn utf16_to_utf8() {
        let text = "hello world";

        assert_eq!(offset_utf16_to_utf8_str(text, 0), 0);
        assert_eq!(offset_utf16_to_utf8_str("", 0), 0);

        assert_eq!(offset_utf16_to_utf8_str("", 1), 0);

        assert_eq!(offset_utf16_to_utf8_str("h", 0), 0);
        assert_eq!(offset_utf16_to_utf8_str("h", 1), 1);

        assert_eq!(offset_utf16_to_utf8_str(text, text.len()), text.len());

        assert_eq!(
            offset_utf16_to_utf8_str(text, text.len() - 1),
            text.len() - 1
        );

        assert_eq!(offset_utf16_to_utf8_str(text, text.len() + 1), text.len());

        assert_eq!(offset_utf16_to_utf8_str("×", 0), 0);
        assert_eq!(offset_utf16_to_utf8_str("×", 1), 2);
        assert_eq!(offset_utf16_to_utf8_str("a×", 0), 0);
        assert_eq!(offset_utf16_to_utf8_str("a×", 1), 1);
        assert_eq!(offset_utf16_to_utf8_str("a×", 2), 3);
        assert_eq!(offset_utf16_to_utf8_str("×a", 1), 2);
        assert_eq!(offset_utf16_to_utf8_str("×a", 2), 3);
    }

    // --- 3-byte UTF-8 (CJK characters) ---

    #[test]
    fn utf8_to_utf16_cjk() {
        // '中' is U+4E2D, 3 bytes in UTF-8, 1 code unit in UTF-16
        let text = "a中b";
        assert_eq!(offset_utf8_to_utf16_str(text, 0), 0); // before 'a'
        assert_eq!(offset_utf8_to_utf16_str(text, 1), 1); // before '中' (byte 1)
        assert_eq!(offset_utf8_to_utf16_str(text, 2), 2); // inside '中' -> snaps to char boundary
        assert_eq!(offset_utf8_to_utf16_str(text, 3), 2); // still inside '中'
        assert_eq!(offset_utf8_to_utf16_str(text, 4), 2); // before 'b'
        assert_eq!(offset_utf8_to_utf16_str(text, 5), 3); // end
    }

    #[test]
    fn utf16_to_utf8_cjk() {
        // '中' is U+4E2D, 3 bytes in UTF-8, 1 code unit in UTF-16
        let text = "a中b";
        assert_eq!(offset_utf16_to_utf8_str(text, 0), 0); // before 'a'
        assert_eq!(offset_utf16_to_utf8_str(text, 1), 1); // before '中'
        assert_eq!(offset_utf16_to_utf8_str(text, 2), 4); // before 'b'
        assert_eq!(offset_utf16_to_utf8_str(text, 3), 5); // end
    }

    // --- 4-byte UTF-8 (emoji / supplementary plane, surrogate pairs in UTF-16) ---

    #[test]
    fn utf8_to_utf16_emoji() {
        // '😀' is U+1F600, 4 bytes in UTF-8, 2 code units in UTF-16 (surrogate pair)
        let text = "a😀b";
        assert_eq!(offset_utf8_to_utf16_str(text, 0), 0); // before 'a'
        assert_eq!(offset_utf8_to_utf16_str(text, 1), 1); // before '😀'
        // offsets 2,3,4 are inside '😀' -> snap to start of char
        assert_eq!(offset_utf8_to_utf16_str(text, 2), 3);
        assert_eq!(offset_utf8_to_utf16_str(text, 3), 3);
        assert_eq!(offset_utf8_to_utf16_str(text, 4), 3);
        assert_eq!(offset_utf8_to_utf16_str(text, 5), 3); // before 'b'
        assert_eq!(offset_utf8_to_utf16_str(text, 6), 4); // end
    }

    #[test]
    fn utf16_to_utf8_emoji() {
        // '😀' is U+1F600, 4 bytes in UTF-8, 2 code units in UTF-16
        let text = "a😀b";
        assert_eq!(offset_utf16_to_utf8_str(text, 0), 0); // before 'a'
        assert_eq!(offset_utf16_to_utf8_str(text, 1), 1); // before '😀'
        // offset 2 is inside the surrogate pair -> snaps
        assert_eq!(offset_utf16_to_utf8_str(text, 2), 5);
        assert_eq!(offset_utf16_to_utf8_str(text, 3), 5); // before 'b'
        assert_eq!(offset_utf16_to_utf8_str(text, 4), 6); // end
    }

    // --- Roundtrip tests ---

    #[test]
    fn roundtrip_ascii() {
        let text = "hello world";
        for i in 0..=text.len() {
            let utf16 = offset_utf8_to_utf16_str(text, i);
            let back = offset_utf16_to_utf8_str(text, utf16);
            assert_eq!(back, i, "roundtrip failed for ascii offset {i}");
        }
    }

    #[test]
    fn roundtrip_mixed_multibyte() {
        // Mix of 1-byte (ASCII), 2-byte (×), 3-byte (中), 4-byte (😀)
        let text = "a×中😀z";
        // Valid utf8 char boundaries
        let boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let mut boundaries_with_end = boundaries.clone();
        boundaries_with_end.push(text.len());

        for &offset in &boundaries_with_end {
            let utf16 = offset_utf8_to_utf16_str(text, offset);
            let back = offset_utf16_to_utf8_str(text, utf16);
            assert_eq!(
                back, offset,
                "roundtrip failed for offset {offset} (utf16={utf16})"
            );
        }
    }

    #[test]
    fn roundtrip_utf16_direction() {
        // Verify utf16->utf8->utf16 roundtrip at char boundaries
        let text = "a×中😀z";
        // UTF-16 length: a(1) + ×(1) + 中(1) + 😀(2) + z(1) = 6
        let utf16_len = text.chars().map(|c| c.len_utf16()).sum::<usize>();
        assert_eq!(utf16_len, 6);

        // Check at each valid utf16 boundary
        // Valid utf16 boundaries: 0, 1, 2, 3, 5, 6 (not 4, which is inside surrogate pair)
        let valid_utf16_boundaries = [0usize, 1, 2, 3, 5, 6];
        for &utf16_off in &valid_utf16_boundaries {
            let utf8 = offset_utf16_to_utf8_str(text, utf16_off);
            let back = offset_utf8_to_utf16_str(text, utf8);
            assert_eq!(
                back, utf16_off,
                "utf16 roundtrip failed for utf16 offset {utf16_off} (utf8={utf8})"
            );
        }
    }
}
