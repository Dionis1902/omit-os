// SPDX-License-Identifier: GPL-2.0-only
//! Translation of Windows paths.
//!
//! Omit OS has a single directory tree, which Windows programs see as
//! drive `C:`. `C:\Users\me\a.txt` is `/Users/me/a.txt`; a path without a
//! drive is used as it is, with `\` turned into `/`, so relative paths
//! stay relative to the working directory. Names are case-sensitive, as
//! everywhere else in the system — a deliberate difference from Windows,
//! where `A.TXT` and `a.txt` are the same file.

use alloc::string::String;

use crate::error::{ERROR_BAD_NETPATH, ERROR_INVALID_NAME, ERROR_PATH_NOT_FOUND};

/// What a Windows path refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A file or directory, as an Omit OS path.
    File(String),
    /// The console (`CON`, `CONIN$`, `CONOUT$`).
    Console,
    /// The null device (`NUL`).
    Null,
}

/// Characters Windows does not allow in file names.
fn forbidden(c: char) -> bool {
    matches!(c, '<' | '>' | '"' | '|' | '?' | '*') || c < ' '
}

/// Translates `path`; on failure returns the Win32 error code.
pub fn translate(path: &str) -> Result<Target, u32> {
    if path.is_empty() {
        return Err(ERROR_PATH_NOT_FOUND);
    }
    // `\\?\C:\x` is the "verbatim" form of `C:\x`.
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    // `\\.\NUL` is the device namespace form of `NUL`.
    let device = path.strip_prefix(r"\\.\").unwrap_or(path);
    for (name, target) in [
        ("NUL", Target::Null),
        ("CON", Target::Console),
        ("CONIN$", Target::Console),
        ("CONOUT$", Target::Console),
    ] {
        if device.eq_ignore_ascii_case(name) {
            return Ok(target);
        }
    }

    if path.starts_with(r"\\") || path.starts_with("//") {
        // `\\server\share`: there is no network. (Unknown devices under
        // `\\.\` end up here as well.)
        return Err(ERROR_BAD_NETPATH);
    }

    let mut rest = path;
    let mut absolute = false;
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        if !bytes[0].eq_ignore_ascii_case(&b'C') {
            // Only drive C: exists.
            return Err(ERROR_PATH_NOT_FOUND);
        }
        rest = &path[2..];
        // `C:file` (relative to the current directory of drive C:) and
        // `C:\file` both make sense here, since there is only one drive.
        absolute = rest.starts_with(['\\', '/']);
    }
    if rest.contains(':') || rest.chars().any(forbidden) {
        return Err(ERROR_INVALID_NAME);
    }

    let mut native = String::with_capacity(rest.len() + 1);
    if absolute && !rest.starts_with(['\\', '/']) {
        native.push('/');
    }
    for c in rest.chars() {
        native.push(if c == '\\' { '/' } else { c });
    }
    if native.is_empty() {
        // `C:` alone: the current directory.
        native.push('.');
    }
    Ok(Target::File(native))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> Target {
        Target::File(path.into())
    }

    #[test]
    fn translates_drive_and_relative_paths() {
        assert_eq!(translate(r"C:\Users\me\a.txt"), Ok(file("/Users/me/a.txt")));
        assert_eq!(translate(r"c:\a"), Ok(file("/a")));
        assert_eq!(translate("C:/a/b"), Ok(file("/a/b")));
        assert_eq!(translate(r"C:\"), Ok(file("/")));
        assert_eq!(translate(r"\a\b"), Ok(file("/a/b")));
        assert_eq!(translate(r"a\b.txt"), Ok(file("a/b.txt")));
        assert_eq!(translate(r"..\x"), Ok(file("../x")));
        assert_eq!(translate("plain"), Ok(file("plain")));
        assert_eq!(translate("C:rel"), Ok(file("rel")));
        assert_eq!(translate("C:"), Ok(file(".")));
        assert_eq!(translate(r"\\?\C:\a"), Ok(file("/a")));
        // Unix-style paths work too.
        assert_eq!(translate("/bin/ls"), Ok(file("/bin/ls")));
    }

    #[test]
    fn recognises_devices() {
        assert_eq!(translate("NUL"), Ok(Target::Null));
        assert_eq!(translate("nul"), Ok(Target::Null));
        assert_eq!(translate(r"\\.\NUL"), Ok(Target::Null));
        assert_eq!(translate("CON"), Ok(Target::Console));
        assert_eq!(translate("CONOUT$"), Ok(Target::Console));
        assert_eq!(translate("conin$"), Ok(Target::Console));
        // Only the whole name is a device.
        assert_eq!(translate("NULL"), Ok(file("NULL")));
    }

    #[test]
    fn rejects_what_does_not_exist_here() {
        assert_eq!(translate(""), Err(ERROR_PATH_NOT_FOUND));
        assert_eq!(translate(r"D:\a"), Err(ERROR_PATH_NOT_FOUND));
        assert_eq!(translate(r"\\server\share\a"), Err(ERROR_BAD_NETPATH));
        assert_eq!(translate("a?b"), Err(ERROR_INVALID_NAME));
        assert_eq!(translate("a*"), Err(ERROR_INVALID_NAME));
        assert_eq!(translate("a\nb"), Err(ERROR_INVALID_NAME));
        assert_eq!(translate(r"C:\a:b"), Err(ERROR_INVALID_NAME));
        assert_eq!(translate("file:stream"), Err(ERROR_INVALID_NAME));
    }
}
