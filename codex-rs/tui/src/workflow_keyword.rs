use regex::Regex;
use std::ops::Range;
use std::sync::LazyLock;

/// Tracks eligible workflow keywords and suppression of one editable user draft.
/// The composer decides whether its current input mode is eligible.
#[derive(Default)]
pub(crate) struct KeywordDraft {
    occurrences: Vec<Range<usize>>,
    dismissed: bool,
}

impl KeywordDraft {
    pub(crate) fn update(&mut self, text: &str) {
        self.occurrences = keyword_ranges(text);
        if self.occurrences.is_empty() {
            self.dismissed = false;
        }
    }

    pub(crate) fn active_ranges(&self) -> Vec<Range<usize>> {
        if self.dismissed {
            Vec::new()
        } else {
            self.occurrences.clone()
        }
    }

    pub(crate) fn submission_opt_in(&self) -> Option<bool> {
        (!self.occurrences.is_empty()).then_some(!self.dismissed)
    }

    pub(crate) fn dismiss_at_cursor(&mut self, cursor: usize) -> bool {
        if !self.dismissed && self.occurrences.iter().any(|range| range.end == cursor) {
            self.dismissed = true;
            true
        } else {
            false
        }
    }

    pub(crate) fn dismiss_all(&mut self) {
        self.dismissed = !self.occurrences.is_empty();
    }

    pub(crate) fn toggle_dismissal(&mut self) -> Option<bool> {
        if self.occurrences.is_empty() {
            return None;
        }
        self.dismissed = !self.dismissed;
        Some(self.dismissed)
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

fn unicode_word(character: char) -> bool {
    static WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}_]$").expect("valid static word pattern"));
    // The reference checks a single UTF-16 code unit at these boundaries.
    character.len_utf16() == 1 && WORD.is_match(character.encode_utf8(&mut [0; 4]))
}

fn closed_literal_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut literal: Option<(char, usize)> = None;
    for (index, character) in text.char_indices() {
        let following = text[index + character.len_utf8()..].chars().next();
        if let Some((opening, start)) = literal {
            if opening == '[' && character == '[' {
                literal = Some(('[', index));
                continue;
            }
            let closing = match opening {
                '<' => '>',
                '{' => '}',
                '[' => ']',
                '(' => ')',
                quote => quote,
            };
            if character == closing && !(opening == '\'' && following.is_some_and(unicode_word)) {
                ranges.push(start..index + character.len_utf8());
                literal = None;
            }
        } else {
            let opens = match character {
                '<' => following.is_some_and(|value| value.is_ascii_alphabetic() || value == '/'),
                '\'' => !text[..index].chars().next_back().is_some_and(unicode_word),
                '`' | '"' | '{' | '[' | '(' => true,
                _ => false,
            };
            if opens {
                literal = Some((character, index));
            }
        }
    }
    ranges
}

fn keyword_ranges(text: &str) -> Vec<Range<usize>> {
    if text.starts_with('/') {
        return Vec::new();
    }
    let literals = closed_literal_ranges(text);
    let ascii_word = |value: char| value.is_ascii_alphanumeric() || value == '_';
    text.to_ascii_lowercase()
        .match_indices("ultracode")
        .filter_map(|(start, token)| {
            let end = start + token.len();
            let before = text[..start].chars().next_back();
            let after = text[end..].chars().next();
            if before.is_some_and(ascii_word)
                || after.is_some_and(ascii_word)
                || literals.iter().any(|range| range.contains(&start))
                || before.is_some_and(|value| matches!(value, '/' | '\\' | '-'))
                || after.is_some_and(|value| matches!(value, '/' | '\\' | '-' | '?'))
                || (after == Some('.') && text[end + 1..].chars().next().is_some_and(unicode_word))
            {
                None
            } else {
                Some(start..end)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_matches_use_ascii_boundaries_and_utf8_byte_ranges() {
        let mut draft = KeywordDraft::default();
        draft.update("é: ULTRACODE, UltraCode!");
        assert_eq!(draft.active_ranges(), vec![4..13, 15..24]);
        draft.update("λultracode ultracodeλ");
        assert_eq!(draft.active_ranges(), vec![2..11, 12..21]);
    }

    #[test]
    fn backspace_after_an_active_token_dismisses_the_entire_draft() {
        let mut draft = KeywordDraft::default();
        draft.update("ultracode and ultracode");
        assert!(!draft.dismiss_at_cursor(8));
        assert!(draft.dismiss_at_cursor(9));
        assert!(!draft.dismiss_at_cursor(9));
        assert!(draft.active_ranges().is_empty());
    }

    #[test]
    fn dismiss_all_keeps_existing_occurrences_dismissed_after_unrelated_edits() {
        let mut draft = KeywordDraft::default();
        draft.update("use ultracode here");
        draft.dismiss_all();
        draft.update("please use ultracode here");
        assert!(draft.active_ranges().is_empty());
        draft.update("please use ultracode here, then ultracode");
        assert!(draft.active_ranges().is_empty());
    }

    #[test]
    fn editing_the_token_rearms_it_and_clearing_resets_the_draft() {
        let mut draft = KeywordDraft::default();
        draft.update("ultracode");
        draft.dismiss_all();
        draft.update("ultracod");
        draft.update("ultracode");
        assert_eq!(draft.active_ranges(), vec![0..9]);
        draft.dismiss_all();
        draft.clear();
        draft.update("ultracode");
        assert_eq!(draft.active_ranges(), vec![0..9]);
    }

    #[test]
    fn unicode_edits_do_not_split_or_rearm_an_unchanged_token() {
        let mut draft = KeywordDraft::default();
        draft.update("🚀 ultracode");
        draft.dismiss_all();
        draft.update("✅ 🚀 ultracode");
        assert!(draft.active_ranges().is_empty());
        draft.update("✅ ultracode");
        assert!(draft.active_ranges().is_empty());
    }

    #[test]
    fn quoted_command_path_and_filename_uses_are_not_triggers() {
        for text in [
            "/say ultracode",
            "`ultracode`",
            "\"ULTRACODE\"",
            "'ultracode'",
            "(ultracode)",
            "[ultracode]",
            "{ultracode}",
            "<ultracode>",
            "src/ultracode",
            r"src\ultracode",
            "pre-ultracode",
            "ultracode-post",
            "ultracode?",
            "ultracode.md",
            "ultracode.λ",
            "ultracode_",
            "_ultracode",
        ] {
            let mut draft = KeywordDraft::default();
            draft.update(text);
            assert!(
                draft.active_ranges().is_empty(),
                "unexpected trigger: {text}"
            );
        }
    }

    #[test]
    fn unclosed_quotes_and_non_tag_angles_remain_eligible() {
        for text in [
            "ultracode.",
            "< ultracode >",
            "don't ultracode",
            "'ultracode",
            "[ultracode",
        ] {
            let mut draft = KeywordDraft::default();
            draft.update(text);
            assert_eq!(draft.active_ranges().len(), 1, "missing trigger: {text}");
        }
    }

    #[test]
    fn keyboard_toggle_restores_the_same_draft() {
        let mut draft = KeywordDraft::default();
        assert_eq!(draft.toggle_dismissal(), None);
        draft.update("ULTRACODE");
        assert_eq!(draft.toggle_dismissal(), Some(true));
        assert!(draft.active_ranges().is_empty());
        assert_eq!(draft.toggle_dismissal(), Some(false));
        assert_eq!(draft.active_ranges(), vec![0..9]);
    }

    #[test]
    fn filename_word_check_preserves_reference_code_unit_semantics() {
        assert!(keyword_ranges("ultracode.λ").is_empty());
        assert_eq!(keyword_ranges("ultracode.𐐀"), vec![0..9]);
        assert_eq!(keyword_ranges("ultracode.\u{0345}"), vec![0..9]);
    }
}
