// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
// Sources: Microsoft Learn "FindFirstFileA" (wildcards); the matching algorithm is
//   our own. Behaviour tests: the unit tests below; wintest.exe.
//! File name patterns, as `FindFirstFile` takes them.
//!
//! `*` stands for any run of characters, including none; `?` for exactly
//! one character. Letters match without regard to case, as file names do
//! on Windows (ASCII letters only here). One piece of history is kept
//! because programs rely on it: the pattern `*.*` matches every name,
//! also names without a dot. The other historical oddities of Windows
//! matching (`?` matching nothing at the end of a name, the `<`, `>` and
//! `"` wildcards, short names) are not reproduced.

/// Whether `name` matches `pattern`.
pub fn matches(pattern: &str, name: &str) -> bool {
    if pattern == "*.*" {
        return true;
    }
    let pattern: alloc::vec::Vec<char> = pattern.chars().collect();
    let name: alloc::vec::Vec<char> = name.chars().collect();
    // The classic two-pointer match with one remembered `*`: on a
    // mismatch, let the last `*` take one more character and try again.
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(&wanted) if wanted == '?' || wanted.eq_ignore_ascii_case(&name[n]) => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((star_p, star_n)) => {
                    p = star_p + 1;
                    n = star_n + 1;
                    star = Some((star_p, star_n + 1));
                }
                None => return false,
            },
        }
    }
    // What is left of the pattern must be able to match nothing.
    pattern[p..].iter().all(|&c| c == '*')
}

/// Whether `text` contains a wildcard.
pub fn has_wildcards(text: &str) -> bool {
    text.contains(['*', '?'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_names_ignore_case() {
        assert!(matches("readme.txt", "readme.txt"));
        assert!(matches("README.TXT", "readme.txt"));
        assert!(!matches("readme.txt", "readme.tx"));
        assert!(!matches("readme.tx", "readme.txt"));
        assert!(matches("", ""));
        assert!(!matches("", "a"));
        assert!(!matches("a", ""));
    }

    #[test]
    fn question_mark_is_exactly_one_character() {
        assert!(matches("file?.txt", "file1.txt"));
        assert!(matches("file?.txt", "fileé.txt"));
        assert!(!matches("file?.txt", "file.txt"));
        assert!(!matches("file?.txt", "file12.txt"));
        assert!(matches("???", "abc"));
        assert!(!matches("???", "ab"));
    }

    #[test]
    fn star_is_any_run() {
        assert!(matches("*", ""));
        assert!(matches("*", "anything.at.all"));
        assert!(matches("*.txt", "a.txt"));
        assert!(matches("*.txt", ".txt"));
        assert!(matches("*.TXT", "notes.txt"));
        assert!(!matches("*.txt", "a.txt.bak"));
        assert!(matches("a*", "a"));
        assert!(matches("a*b*c", "aXXbYYc"));
        assert!(matches("a*b*c", "abc"));
        assert!(!matches("a*b*c", "aXXbYY"));
        assert!(matches("*a*", "banana"));
        assert!(matches("**a**", "a"));
        // Backtracking: the first `b` is not the one that matters.
        assert!(matches("*b?d", "abcbcd"));
        assert!(!matches("*b?d", "abcbce"));
    }

    #[test]
    fn star_dot_star_matches_everything() {
        assert!(matches("*.*", "with.dot"));
        assert!(matches("*.*", "nodot"));
        assert!(matches("*.*", ""));
        // But only that exact pattern is special.
        assert!(!matches("a*.*", "abc"));
        assert!(matches("a*.*", "abc.d"));
    }

    #[test]
    fn wildcard_detection() {
        assert!(has_wildcards("*.txt"));
        assert!(has_wildcards("a?c"));
        assert!(!has_wildcards("plain.txt"));
    }
}
