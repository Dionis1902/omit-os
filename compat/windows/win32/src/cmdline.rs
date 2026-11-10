// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows command lines.
//!
//! A Windows program receives one string and splits it into arguments
//! itself, by rules documented for `CommandLineToArgvW` and the C runtime:
//! arguments are separated by spaces or tabs; double quotes group; a
//! backslash is literal unless a run of them precedes a double quote, in
//! which case each pair stands for one backslash and an odd one out
//! escapes the quote. [`build`] produces a string that splits back into
//! exactly the given arguments; [`split`] implements the rules, mainly so
//! the tests can check that.

use alloc::string::String;
use alloc::vec::Vec;

/// Joins `arguments` into one command line.
pub fn build<S: AsRef<str>>(arguments: &[S]) -> String {
    let mut line = String::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        quote(argument.as_ref(), &mut line);
    }
    line
}

fn quote(argument: &str, line: &mut String) {
    let plain = !argument.is_empty() && !argument.contains([' ', '\t', '"']);
    if plain {
        line.push_str(argument);
        return;
    }
    line.push('"');
    let mut backslashes = 0;
    for c in argument.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // The backslashes before a quote must be doubled, and the
                // quote itself escaped.
                for _ in 0..backslashes + 1 {
                    line.push('\\');
                }
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
        line.push(c);
    }
    // Likewise for backslashes before the closing quote.
    for _ in 0..backslashes {
        line.push('\\');
    }
    line.push('"');
}

/// Splits a command line into arguments.
pub fn split(line: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut in_argument = false;
    let mut quoted = false;
    let mut backslashes = 0usize;
    for c in line.chars() {
        if c == '\\' {
            backslashes += 1;
            in_argument = true;
            continue;
        }
        if c == '"' {
            for _ in 0..backslashes / 2 {
                current.push('\\');
            }
            if backslashes % 2 == 1 {
                current.push('"');
            } else {
                quoted = !quoted;
            }
            backslashes = 0;
            in_argument = true;
            continue;
        }
        for _ in 0..backslashes {
            current.push('\\');
        }
        backslashes = 0;
        if (c == ' ' || c == '\t') && !quoted {
            if in_argument {
                arguments.push(core::mem::take(&mut current));
                in_argument = false;
            }
        } else {
            current.push(c);
            in_argument = true;
        }
    }
    for _ in 0..backslashes {
        current.push('\\');
    }
    if in_argument {
        arguments.push(current);
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    #[test]
    fn builds_readable_lines() {
        assert_eq!(build(&["prog.exe", "a", "b"]), "prog.exe a b");
        assert_eq!(build(&["prog.exe", "two words"]), r#"prog.exe "two words""#);
        assert_eq!(build(&["p", ""]), r#"p """#);
        assert_eq!(build(&["p", r"C:\dir\file"]), r"p C:\dir\file");
        assert_eq!(build(&["p", r#"say "hi""#]), r#"p "say \"hi\"""#);
        assert_eq!(build(&["p", r"dir with space\"]), r#"p "dir with space\\""#);
        assert_eq!(build::<&str>(&[]), "");
    }

    #[test]
    fn splits_by_the_documented_rules() {
        // The examples from the C runtime documentation.
        assert_eq!(split(r#""abc" d e"#), ["abc", "d", "e"]);
        assert_eq!(split(r#"a\\b d"e f"g h"#), [r"a\\b", "de fg", "h"]);
        assert_eq!(split(r#"a\\\"b c d"#), [r#"a\"b"#, "c", "d"]);
        assert_eq!(split(r#"a\\\\"b c" d e"#), [r"a\\b c", "d", "e"]);
        assert_eq!(split("  a \t b  "), ["a", "b"]);
        assert_eq!(split(r#"a "" b"#), ["a", "", "b"]);
        assert!(split("").is_empty());
    }

    #[test]
    fn building_then_splitting_gives_the_arguments_back() {
        let cases = [
            vec!["prog.exe"],
            vec!["prog.exe", "a b", "", "c"],
            vec!["p", r"\", r"\\", r#"""#, r#"\""#, r#"\\""#, r#"a"b"#],
            vec!["p", r"trailing\", r"trailing space \", "tab\there"],
            vec!["p", r#"mixed \\"quotes\" and \\ slashes\\"#],
            vec!["p", "unicode ☃ ok", "'single'"],
        ];
        for case in cases {
            assert_eq!(split(&build(&case)), case, "{case:?}");
        }
    }
}
