// SPDX-License-Identifier: Apache-2.0
//! The layer that runs *before* YAML.
//!
//! A `.3dbv`/`.3dbx` file is not plain YAML. It carries a `#!define` macro preprocessor and an
//! `include` list resolved relative to the including file, and path values may contain globs.
//! Handing the raw text to a YAML parser therefore parses the wrong document — `#!define` lines
//! are not comments (YAML comments are `#`, and `#!define` happens to look like one, so a YAML
//! parser silently *drops* them rather than failing) and every macro reference survives
//! unexpanded into the values.
//!
//! That silence is the reason this is a separate, tested step: the failure mode is a file that
//! parses "successfully" into wrong paths.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Expand `#!define NAME VALUE` macros and strip the directives.
///
/// Longest-name-first so that a `NAME` which is a prefix of another cannot shadow it — with
/// `PATH` and `PATH_A` defined, naive ordering rewrites the second into `<PATH>_A`.
pub(crate) fn expand_defines(text: &str) -> String {
    let mut defines: BTreeMap<String, String> = BTreeMap::new();
    let mut body = String::with_capacity(text.len());
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("#!define") {
            let mut it = rest.trim().splitn(2, char::is_whitespace);
            if let (Some(name), Some(value)) = (it.next(), it.next()) {
                defines.insert(name.to_string(), value.trim().to_string());
            }
            continue; // the directive itself is not part of the document
        }
        body.push_str(line);
        body.push('\n');
    }
    let mut keys: Vec<&String> = defines.keys().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    for k in keys {
        body = body.replace(k.as_str(), &defines[k]);
    }
    body
}

/// Resolve a path named inside `from`, relative to that file's directory.
pub(crate) fn relative_to(from: &Path, target: &str) -> PathBuf {
    let p = Path::new(target);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    from.parent().unwrap_or(Path::new(".")).join(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defines_are_expanded_and_the_directives_removed() {
        let out = expand_defines("#!define P ../ng45\nHeader:\n  f: P/tech.lef\n");
        assert!(!out.contains("#!define"), "the directive must not reach the YAML parser");
        assert!(out.contains("../ng45/tech.lef"), "got: {out}");
    }

    #[test]
    fn a_longer_name_is_not_shadowed_by_a_shorter_prefix_of_it() {
        // The bug this ordering exists to prevent: with P defined first, P_EXTRA becomes
        // "<P>_EXTRA" and the second macro never matches.
        let out = expand_defines("#!define P a\n#!define P_EXTRA b\nx: P_EXTRA\ny: P\n");
        assert!(out.contains("x: b"), "got: {out}");
        assert!(out.contains("y: a"), "got: {out}");
    }

    #[test]
    fn a_file_without_defines_is_unchanged_apart_from_line_endings() {
        let out = expand_defines("Header:\n  version: 1.0\n");
        assert_eq!(out, "Header:\n  version: 1.0\n");
    }

    #[test]
    fn includes_resolve_against_the_including_files_directory() {
        let p = relative_to(Path::new("/a/b/top.3dbx"), "defs.3dbv");
        assert_eq!(p, Path::new("/a/b/defs.3dbv"));
        assert_eq!(relative_to(Path::new("/a/b/top.3dbx"), "/x/y.3dbv"), Path::new("/x/y.3dbv"));
    }
}
