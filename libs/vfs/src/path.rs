// SPDX-License-Identifier: GPL-2.0-only
//! Path handling.
//!
//! The VFS only accepts *normalised* absolute paths: they start with `/`,
//! have no empty, `.` or `..` components and no trailing slash (except the
//! root itself, `/`). [`normalize`] produces that form from arbitrary user
//! input.
//!
//! `..` is resolved lexically. That is exact as long as there are no
//! symbolic links, which the VFS does not have.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{FsError, FsResult, NAME_MAX};

/// Longest permitted path, in bytes.
pub const PATH_MAX: usize = 4096;

/// Turns `path`, interpreted relative to the normalised absolute directory
/// `cwd` unless it starts with `/`, into a normalised absolute path.
pub fn normalize(cwd: &str, path: &str) -> FsResult<String> {
    if path.is_empty() {
        return Err(FsError::NotFound);
    }
    let mut parts: Vec<&str> = Vec::new();
    let sources = if path.starts_with('/') {
        ["", path]
    } else {
        [cwd, path]
    };
    for source in sources {
        for component in source.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                name => {
                    validate_name(name)?;
                    parts.push(name);
                }
            }
        }
    }

    let mut result = String::new();
    for part in &parts {
        result.push('/');
        result.push_str(part);
    }
    if result.is_empty() {
        result.push('/');
    }
    if result.len() > PATH_MAX {
        return Err(FsError::InvalidName);
    }
    Ok(result)
}

/// Checks that `name` can be used as a single path component.
pub fn validate_name(name: &str) -> FsResult<()> {
    if name.is_empty()
        || name.len() > NAME_MAX
        || name == "."
        || name == ".."
        || name.bytes().any(|b| b == b'/' || b == 0)
    {
        return Err(FsError::InvalidName);
    }
    Ok(())
}

/// Iterates over the components of a normalised absolute path. `/` has
/// none. Rejects paths that are not normalised.
pub fn components(path: &str) -> FsResult<impl Iterator<Item = &str>> {
    let rest = path.strip_prefix('/').ok_or(FsError::InvalidArgument)?;
    if !rest.is_empty() {
        for component in rest.split('/') {
            match component {
                "" | "." | ".." => return Err(FsError::InvalidArgument),
                name => validate_name(name)?,
            }
        }
    }
    Ok(rest.split('/').filter(|c| !c.is_empty()))
}

/// Splits a normalised absolute path into its parent directory and final
/// component. Returns `None` for `/`.
pub fn split(path: &str) -> FsResult<Option<(&str, &str)>> {
    // Validates the whole path as a side effect.
    let _ = components(path)?;
    if path == "/" {
        return Ok(None);
    }
    let index = path.rfind('/').ok_or(FsError::InvalidArgument)?;
    let parent = if index == 0 { "/" } else { &path[..index] };
    Ok(Some((parent, &path[index + 1..])))
}

/// The final component of a normalised absolute path, or `None` for `/`.
pub fn file_name(path: &str) -> Option<&str> {
    split(path).ok().flatten().map(|(_, name)| name)
}

/// Joins a normalised absolute directory path and a single component.
pub fn join(dir: &str, name: &str) -> String {
    let mut joined = String::from(dir);
    if !joined.ends_with('/') {
        joined.push('/');
    }
    joined.push_str(name);
    joined
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[test]
    fn normalizes_relative_and_absolute_paths() {
        let n = |cwd, p| normalize(cwd, p).unwrap();
        assert_eq!(n("/", "a"), "/a");
        assert_eq!(n("/x/y", "a/b"), "/x/y/a/b");
        assert_eq!(n("/x/y", "/a"), "/a");
        assert_eq!(n("/x/y", ".."), "/x");
        assert_eq!(n("/x/y", "../.."), "/");
        assert_eq!(n("/x/y", "../../../.."), "/", "cannot climb above the root");
        assert_eq!(n("/x", "./a/./b/"), "/x/a/b");
        assert_eq!(n("/x", "a//b///"), "/x/a/b");
        assert_eq!(n("/x", "/"), "/");
        assert_eq!(n("/x", "."), "/x");
        assert_eq!(n("/", "a/../b"), "/b");
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(normalize("/", ""), Err(FsError::NotFound));
        assert_eq!(normalize("/", "a\0b"), Err(FsError::InvalidName));
        let long = "x".repeat(NAME_MAX + 1);
        assert_eq!(normalize("/", &long), Err(FsError::InvalidName));
        assert!(normalize("/", &"x".repeat(NAME_MAX)).is_ok());
    }

    #[test]
    fn components_and_split() {
        let c = |p| components(p).map(|i| i.collect::<Vec<_>>());
        assert_eq!(c("/"), Ok(Vec::new()));
        assert_eq!(c("/a/b"), Ok(Vec::from(["a", "b"])));
        assert_eq!(c("a/b"), Err(FsError::InvalidArgument));
        assert_eq!(c("/a/"), Err(FsError::InvalidArgument));
        assert_eq!(c("/a/./b"), Err(FsError::InvalidArgument));

        assert_eq!(split("/"), Ok(None));
        assert_eq!(split("/a"), Ok(Some(("/", "a"))));
        assert_eq!(split("/a/b/c"), Ok(Some(("/a/b", "c"))));
        assert_eq!(file_name("/a/b"), Some("b"));
        assert_eq!(file_name("/"), None);
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/a", "b"), "/a/b");
    }
}
