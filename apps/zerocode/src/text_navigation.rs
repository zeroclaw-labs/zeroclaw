use unicode_segmentation::UnicodeSegmentation;

fn is_word_grapheme(grapheme: &str) -> bool {
    grapheme
        .chars()
        .any(|character| character == '_' || character.is_alphanumeric())
}

pub(crate) fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    let mut previous = 0;
    for (start, grapheme) in text.grapheme_indices(true) {
        let end = start + grapheme.len();
        if cursor <= end {
            return if cursor == start { previous } else { start };
        }
        previous = start;
    }
    previous
}

pub(crate) fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    for (start, grapheme) in text.grapheme_indices(true) {
        let end = start + grapheme.len();
        if cursor < end {
            return end;
        }
    }
    text.len()
}

pub(crate) fn normalize_grapheme_cursor(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    if cursor == 0 || cursor == text.len() {
        return cursor;
    }
    for (start, grapheme) in text.grapheme_indices(true) {
        let end = start + grapheme.len();
        if cursor == start || cursor == end {
            return cursor;
        }
        if cursor < end {
            return end;
        }
    }
    text.len()
}

fn previous_safe_cursor(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    if text.is_char_boundary(cursor) {
        let normalized = normalize_grapheme_cursor(text, cursor);
        if normalized == cursor {
            return cursor;
        }
    }
    previous_grapheme_boundary(text, cursor)
}

pub(crate) fn previous_word_boundary(text: &str, cursor: usize) -> usize {
    let cursor = previous_safe_cursor(text, cursor);
    let mut graphemes = text[..cursor].grapheme_indices(true).rev();

    let mut target_is_word = None;
    for (_, grapheme) in graphemes.by_ref() {
        if grapheme.chars().all(char::is_whitespace) {
            continue;
        }
        target_is_word = Some(is_word_grapheme(grapheme));
        break;
    }

    let Some(target_is_word) = target_is_word else {
        return 0;
    };

    for (index, grapheme) in graphemes {
        if grapheme.chars().all(char::is_whitespace) || is_word_grapheme(grapheme) != target_is_word
        {
            return index + grapheme.len();
        }
    }

    0
}

pub(crate) fn next_word_boundary(text: &str, cursor: usize) -> usize {
    let cursor = normalize_grapheme_cursor(text, cursor);
    let mut graphemes = text[cursor..].grapheme_indices(true).peekable();

    while graphemes
        .peek()
        .is_some_and(|(_, grapheme)| grapheme.chars().all(char::is_whitespace))
    {
        graphemes.next();
    }

    let Some((_, first)) = graphemes.next() else {
        return text.len();
    };
    let target_is_word = is_word_grapheme(first);

    for (offset, grapheme) in graphemes {
        if grapheme.chars().all(char::is_whitespace) || is_word_grapheme(grapheme) != target_is_word
        {
            return cursor + offset;
        }
    }

    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boundaries_cross_words_whitespace_and_punctuation() {
        let text = "alpha  beta-gamma";
        assert_eq!(next_word_boundary(text, 0), 5);
        assert_eq!(next_word_boundary(text, 5), 11);
        assert_eq!(next_word_boundary(text, 11), 12);
        assert_eq!(previous_word_boundary(text, text.len()), 12);
        assert_eq!(previous_word_boundary(text, 12), 11);
        assert_eq!(previous_word_boundary(text, 11), 7);
    }

    #[test]
    fn boundaries_preserve_unicode_graphemes() {
        let text = "e\u{301}lan 世界";
        assert_eq!(next_grapheme_boundary(text, 0), "e\u{301}".len());
        assert_eq!(previous_grapheme_boundary(text, "e\u{301}".len()), 0);
        assert_eq!(next_word_boundary(text, 0), "e\u{301}lan".len());
        assert!(text.is_char_boundary(next_word_boundary(text, "e\u{301}".len())));
        assert!(text.is_char_boundary(previous_word_boundary(text, text.len())));
    }

    #[test]
    fn normalization_moves_an_interior_zwj_cursor_to_the_grapheme_end() {
        let text = "👩\u{200d}👩";
        let interior = "👩\u{200d}".len();
        assert_eq!(normalize_grapheme_cursor(text, interior), text.len());
        assert_eq!(previous_grapheme_boundary(text, interior), 0);
        assert_eq!(next_grapheme_boundary(text, interior), text.len());
    }
}
