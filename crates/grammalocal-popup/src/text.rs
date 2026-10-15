//! Pure text helpers: UTF-16 offset conversion and end-to-start edit application.
//! No GTK here, so these run in plain `cargo test`.

/// One replacement. `start` and `len` are in characters (not bytes, not UTF-16 units).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub len: usize,
    pub replacement: String,
}

/// Converts a UTF-16 offset into a character index of `text`.
/// Returns `None` when the offset falls inside a surrogate pair or past the end.
pub fn utf16_to_char(text: &str, u16_off: usize) -> Option<usize> {
    let mut units = 0usize;
    for (ci, c) in text.chars().enumerate() {
        if units == u16_off {
            return Some(ci);
        }
        if units > u16_off {
            return None;
        }
        units += c.len_utf16();
    }
    (units == u16_off).then(|| text.chars().count())
}

/// Converts a UTF-16 span (`offset`, `length`) into a `(char_start, char_len)` pair.
pub fn utf16_span_to_chars(text: &str, offset: usize, length: usize) -> Option<(usize, usize)> {
    let start = utf16_to_char(text, offset)?;
    let end = utf16_to_char(text, offset + length)?;
    Some((start, end - start))
}

/// Applies edits from the end of the text towards the start, so earlier indices stay valid.
/// An edit that overlaps an already applied (later) edit, or that lies outside the text, is skipped.
pub fn apply_edits(text: &str, edits: &[Edit]) -> String {
    let original_len = text.chars().count();
    let mut chars: Vec<char> = text.chars().collect();
    let mut order: Vec<&Edit> = edits.iter().collect();
    order.sort_by_key(|edit| std::cmp::Reverse(edit.start));

    // Start of the last applied edit. Any further edit must end at or before it.
    let mut floor = usize::MAX;
    for edit in order {
        let end = edit.start + edit.len;
        if end > original_len || end > floor {
            continue;
        }
        chars.splice(edit.start..end, edit.replacement.chars());
        floor = edit.start;
    }
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, len: usize, replacement: &str) -> Edit {
        Edit {
            start,
            len,
            replacement: replacement.to_string(),
        }
    }

    #[test]
    fn utf16_ascii_maps_one_to_one() {
        assert_eq!(utf16_to_char("bonjour", 0), Some(0));
        assert_eq!(utf16_to_char("bonjour", 3), Some(3));
        assert_eq!(utf16_to_char("bonjour", 7), Some(7));
        assert_eq!(utf16_to_char("bonjour", 8), None);
    }

    #[test]
    fn utf16_surrogate_pairs_count_two_units() {
        // "a" (1 unit), "😀" (2 units), "b" (1 unit)
        let s = "a😀b";
        assert_eq!(utf16_to_char(s, 0), Some(0));
        assert_eq!(utf16_to_char(s, 1), Some(1));
        assert_eq!(utf16_to_char(s, 2), None, "inside the surrogate pair");
        assert_eq!(utf16_to_char(s, 3), Some(2));
        assert_eq!(utf16_to_char(s, 4), Some(3));
        assert_eq!(utf16_to_char(s, 5), None);
    }

    #[test]
    fn utf16_accented_letters_are_single_units() {
        // "é" is one UTF-16 unit but two bytes in UTF-8.
        let s = "é ça";
        assert_eq!(utf16_to_char(s, 2), Some(2));
        assert_eq!(utf16_span_to_chars(s, 2, 2), Some((2, 2)), "ça");
        assert_eq!(
            utf16_span_to_chars(s, 3, 2),
            None,
            "splits the ç-a boundary past the end"
        );
    }

    #[test]
    fn span_conversion_handles_emoji_before_error() {
        // Error "fautes" starts after an emoji: UTF-16 offset 3, length 6.
        let s = "😀 fautes";
        assert_eq!(utf16_span_to_chars(s, 3, 6), Some((2, 6)));
        assert_eq!(
            utf16_span_to_chars(s, 1, 6),
            None,
            "starts inside the surrogate pair"
        );
    }

    #[test]
    fn edits_apply_end_to_start_regardless_of_input_order() {
        let text = "un chat et un chien";
        let edits = vec![edit(0, 2, "Un"), edit(14, 5, "chiens")];
        assert_eq!(apply_edits(text, &edits), "Un chat et un chiens");
    }

    #[test]
    fn length_changing_replacements_keep_later_offsets_valid() {
        let text = "ab cd ef";
        let edits = vec![edit(0, 2, "XXXXX"), edit(3, 2, "Y"), edit(6, 2, "")];
        assert_eq!(apply_edits(text, &edits), "XXXXX Y ");
    }

    #[test]
    fn overlapping_edits_keep_only_the_later_one() {
        let text = "abcdef";
        let edits = vec![edit(0, 4, "1"), edit(2, 3, "2")];
        // The edit at 2..5 is applied first; 0..4 overlaps it and is skipped.
        assert_eq!(apply_edits(text, &edits), "ab2f");
    }

    #[test]
    fn out_of_range_and_empty_edits_are_ignored() {
        let text = "abc";
        assert_eq!(apply_edits(text, &[edit(5, 1, "x")]), "abc");
        assert_eq!(apply_edits(text, &[]), "abc");
        assert_eq!(apply_edits(text, &[edit(1, 0, "-")]), "a-bc");
    }

    #[test]
    fn edit_on_multibyte_text_uses_char_indices() {
        let text = "é😀 faute";
        // Char indices: é=0, 😀=1, space=2, f=3, a=4...
        let edits = vec![edit(3, 5, "faute corrigée")];
        assert_eq!(apply_edits(text, &edits), "é😀 faute corrigée");
    }
}
