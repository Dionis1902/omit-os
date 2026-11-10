// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Command-line parsing.
//!
//! Supported syntax: words separated by whitespace; single quotes (literal)
//! and double quotes (with `\"` and `\\` escapes); backslash escapes outside
//! quotes; one output redirection, `> file` or `>> file`; one input
//! redirection, `< file`; and pipelines, `first | second | ...`, in which
//! each program's output is the next one's input. There are no variables,
//! globs or command sequences.

use alloc::string::String;
use alloc::vec::Vec;

/// Where a command's standard output goes instead of the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    /// The target file, as typed.
    pub path: String,
    /// `>>` rather than `>`.
    pub append: bool,
}

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandLine {
    /// The command name followed by its arguments. Empty for a blank line.
    /// In a pipeline, the first command.
    pub words: Vec<String>,
    /// The further commands of a pipeline, each as name and arguments.
    pub rest: Vec<Vec<String>>,
    /// Input redirection (`< file`), if any: the file the first command
    /// reads its standard input from.
    pub input: Option<String>,
    /// Output redirection, if any; in a pipeline, of the last command.
    pub redirect: Option<Redirect>,
    /// The line ended with `&`: do not wait for the program to finish.
    pub background: bool,
}

/// Why a line could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// A quote was opened but never closed.
    UnterminatedQuote,
    /// `>` or `>>` is not followed by a file name.
    MissingRedirectTarget,
    /// More than one redirection was given.
    DuplicateRedirect,
    /// The line ends with a lone backslash.
    TrailingBackslash,
    /// `&` appears somewhere other than at the end of the line.
    MisplacedBackground,
    /// `<` is not followed by a file name.
    MissingInputSource,
    /// More than one input redirection was given.
    DuplicateInput,
    /// `|` is not between two commands.
    MissingPipelineCommand,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::UnterminatedQuote => "unterminated quote",
            Self::MissingRedirectTarget => "missing file name after redirection",
            Self::DuplicateRedirect => "more than one output redirection",
            Self::TrailingBackslash => "trailing backslash",
            Self::MisplacedBackground => "'&' is only allowed at the end of a command",
            Self::MissingInputSource => "missing file name after '<'",
            Self::DuplicateInput => "more than one input redirection",
            Self::MissingPipelineCommand => "missing command before or after '|'",
        })
    }
}

enum Token {
    Word(String),
    Redirect { append: bool },
    Input,
    Pipe,
    Background,
}

fn tokenize(line: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    // `Some` while a word is being built; quoting can produce an empty word.
    let mut word: Option<String> = None;

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if let Some(done) = word.take() {
                    tokens.push(Token::Word(done));
                }
            }
            '>' => {
                if let Some(done) = word.take() {
                    tokens.push(Token::Word(done));
                }
                let append = chars.next_if_eq(&'>').is_some();
                tokens.push(Token::Redirect { append });
            }
            '&' | '<' | '|' => {
                if let Some(done) = word.take() {
                    tokens.push(Token::Word(done));
                }
                tokens.push(match c {
                    '&' => Token::Background,
                    '<' => Token::Input,
                    _ => Token::Pipe,
                });
            }
            '\'' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(ParseError::UnterminatedQuote),
                    }
                }
            }
            '"' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => current.push(c),
                            Some(c) => {
                                current.push('\\');
                                current.push(c);
                            }
                            None => return Err(ParseError::UnterminatedQuote),
                        },
                        Some(c) => current.push(c),
                        None => return Err(ParseError::UnterminatedQuote),
                    }
                }
            }
            '\\' => match chars.next() {
                Some(c) => word.get_or_insert_with(String::new).push(c),
                None => return Err(ParseError::TrailingBackslash),
            },
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    if let Some(done) = word {
        tokens.push(Token::Word(done));
    }
    Ok(tokens)
}

/// Parses one line of input.
pub fn parse(line: &str) -> Result<CommandLine, ParseError> {
    let mut command = CommandLine::default();
    let mut tokens = tokenize(line)?.into_iter();
    // The command currently being collected: `command.words` first, then
    // one entry of `command.rest` per `|`.
    let mut current: Vec<String> = Vec::new();
    let mut piped = false;
    while let Some(token) = tokens.next() {
        if command.background {
            return Err(ParseError::MisplacedBackground);
        }
        match token {
            Token::Background => {
                if current.is_empty() {
                    return Err(ParseError::MisplacedBackground);
                }
                command.background = true;
            }
            Token::Word(word) => current.push(word),
            Token::Pipe => {
                if current.is_empty() {
                    return Err(ParseError::MissingPipelineCommand);
                }
                let finished = core::mem::take(&mut current);
                if piped {
                    command.rest.push(finished);
                } else {
                    command.words = finished;
                }
                piped = true;
            }
            Token::Input => {
                let Some(Token::Word(path)) = tokens.next() else {
                    return Err(ParseError::MissingInputSource);
                };
                if command.input.is_some() {
                    return Err(ParseError::DuplicateInput);
                }
                command.input = Some(path);
            }
            Token::Redirect { append } => {
                let Some(Token::Word(path)) = tokens.next() else {
                    return Err(ParseError::MissingRedirectTarget);
                };
                if command.redirect.is_some() {
                    return Err(ParseError::DuplicateRedirect);
                }
                command.redirect = Some(Redirect { path, append });
            }
        }
    }
    if piped {
        if current.is_empty() {
            return Err(ParseError::MissingPipelineCommand);
        }
        command.rest.push(current);
    } else {
        command.words = current;
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        parse(line).unwrap().words
    }

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(words("  ls   -l\t/tmp "), ["ls", "-l", "/tmp"]);
        assert!(words("   ").is_empty());
        assert!(words("").is_empty());
    }

    #[test]
    fn quoting_and_escapes() {
        assert_eq!(words("echo 'a  b' \"c  d\""), ["echo", "a  b", "c  d"]);
        assert_eq!(words("echo a'b c'd"), ["echo", "ab cd"]);
        assert_eq!(
            words(r#"echo "say \"hi\" \\ \n""#),
            ["echo", r#"say "hi" \ \n"#]
        );
        assert_eq!(words(r"echo a\ b \> \'"), ["echo", "a b", ">", "'"]);
        assert_eq!(words("echo '' \"\""), ["echo", "", ""]);
        assert_eq!(words("echo '>'"), ["echo", ">"]);
    }

    #[test]
    fn redirection() {
        let c = parse("echo hi > out.txt").unwrap();
        assert_eq!(c.words, ["echo", "hi"]);
        assert_eq!(
            c.redirect,
            Some(Redirect {
                path: "out.txt".into(),
                append: false
            })
        );
        let c = parse("echo hi>>'my file' more").unwrap();
        assert_eq!(c.words, ["echo", "hi", "more"]);
        assert_eq!(
            c.redirect,
            Some(Redirect {
                path: "my file".into(),
                append: true
            })
        );
    }

    #[test]
    fn background() {
        let c = parse("spin &").unwrap();
        assert_eq!(c.words, ["spin"]);
        assert!(c.background);
        assert!(parse("spin&").unwrap().background);
        assert!(!parse("echo '&'").unwrap().background);
        assert!(!parse("echo a\\&").unwrap().background);
        assert_eq!(parse("&"), Err(ParseError::MisplacedBackground));
        assert_eq!(parse("a & b"), Err(ParseError::MisplacedBackground));
        assert_eq!(parse("a & &"), Err(ParseError::MisplacedBackground));
    }

    #[test]
    fn errors() {
        assert_eq!(parse("echo 'oops"), Err(ParseError::UnterminatedQuote));
        assert_eq!(parse("echo \"oops"), Err(ParseError::UnterminatedQuote));
        assert_eq!(parse("echo \"oops\\"), Err(ParseError::UnterminatedQuote));
        assert_eq!(parse("echo >"), Err(ParseError::MissingRedirectTarget));
        assert_eq!(parse("echo > > f"), Err(ParseError::MissingRedirectTarget));
        assert_eq!(parse("echo > a > b"), Err(ParseError::DuplicateRedirect));
        assert_eq!(parse("echo \\"), Err(ParseError::TrailingBackslash));
    }

    #[test]
    fn parses_pipelines_and_input_redirection() {
        let command = parse("cat < in.txt | tee copy | cat > out.txt").unwrap();
        assert_eq!(command.words, ["cat"]);
        assert_eq!(command.rest, [list(&["tee", "copy"]), list(&["cat"])]);
        assert_eq!(command.input.as_deref(), Some("in.txt"));
        assert_eq!(
            command.redirect,
            Some(Redirect {
                path: "out.txt".into(),
                append: false
            })
        );
        assert!(!command.background);

        // No spaces needed; quotes hide the special characters.
        let command = parse("a|b 'x|y' \"<\"&").unwrap();
        assert_eq!(command.words, ["a"]);
        assert_eq!(command.rest, [list(&["b", "x|y", "<"])]);
        assert!(command.background);

        let command = parse("sort <data").unwrap();
        assert_eq!(command.words, ["sort"]);
        assert!(command.rest.is_empty());
        assert_eq!(command.input.as_deref(), Some("data"));
    }

    #[test]
    fn rejects_malformed_pipelines() {
        for line in ["| a", "a |", "a | | b", "a | & ", "|"] {
            assert!(
                matches!(
                    parse(line),
                    Err(ParseError::MissingPipelineCommand | ParseError::MisplacedBackground)
                ),
                "{line}"
            );
        }
        assert_eq!(parse("a <"), Err(ParseError::MissingInputSource));
        assert_eq!(parse("a < | b"), Err(ParseError::MissingInputSource));
        assert_eq!(parse("a < x < y"), Err(ParseError::DuplicateInput));
    }

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|&item| String::from(item)).collect()
    }
}
