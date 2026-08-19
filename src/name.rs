//! Agent-name resolution and window-name suffix handling.
//!
//! Suffix characters are multibyte UTF-8, so stripping operates over `char`s, never
//! bytes.

const SUFFIX_CHARS: &[char] = &['!', '\u{2713}', '\u{2717}', '?'];

pub fn strip_suffix(name: &str) -> String {
    let mut chars: Vec<char> = name.chars().collect();
    while matches!(chars.last(), Some(c) if SUFFIX_CHARS.contains(c)) {
        chars.pop();
    }
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_suffix_is_unchanged() {
        assert_eq!(strip_suffix("impl"), "impl");
    }

    #[test]
    fn empty_string_is_unchanged() {
        assert_eq!(strip_suffix(""), "");
    }

    #[test]
    fn strips_single_suffix() {
        assert_eq!(strip_suffix("impl!"), "impl");
        assert_eq!(strip_suffix("review\u{2713}"), "review");
        assert_eq!(strip_suffix("tests\u{2717}"), "tests");
    }

    #[test]
    fn strips_repeated_suffix() {
        assert_eq!(strip_suffix("impl!!"), "impl");
        assert_eq!(strip_suffix("impl!\u{2713}?"), "impl");
    }

    #[test]
    fn suffix_only_string_becomes_empty() {
        assert_eq!(strip_suffix("!\u{2713}\u{2717}?"), "");
    }

    #[test]
    fn does_not_touch_interior_characters() {
        assert_eq!(strip_suffix("impl!review"), "impl!review");
    }
}
