use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glob::{glob_with, MatchOptions};

pub const OUTPUT_MARKER: &str = "squish";
pub const OUTPUT_EXTENSION: &str = "mp4";

const VIDEO_EXTENSIONS: [&str; 15] = [
    "mov", "mp4", "m4v", "mkv", "avi", "webm", "wmv", "flv", "mpg", "mpeg", "mts", "m2ts", "ts",
    "mxf", "3gp",
];

/// cmd and PowerShell hand the pattern over untouched, so squish globs itself.
/// A pattern that matches nothing but exists on disk is taken literally.
pub fn expand_patterns(patterns: &[String]) -> Result<Vec<PathBuf>> {
    let options = MatchOptions {
        case_sensitive: !cfg!(windows),
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };

    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for pattern in patterns {
        let mut matched = false;
        let entries = glob_with(&normalize_separators(pattern), options)
            .with_context(|| format!("invalid pattern {pattern:?}"))?;
        for entry in entries {
            let path = entry.with_context(|| format!("cannot read a match of {pattern:?}"))?;
            matched = true;
            if path.is_file() && seen.insert(dedup_key(&path)) {
                files.push(path);
            }
        }
        if !matched {
            let literal = PathBuf::from(pattern);
            if literal.is_file() && seen.insert(dedup_key(&literal)) {
                files.push(literal);
            }
        }
    }
    Ok(files)
}

/// The glob crate always understands `/`; on Windows `\` is a separator too.
fn normalize_separators(pattern: &str) -> String {
    if cfg!(windows) {
        pattern.replace('\\', "/")
    } else {
        pattern.to_owned()
    }
}

fn dedup_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// Every video in `dir`, for when the command line carries no pattern at all.
pub fn videos_in(dir: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?;
    gather(entries, recursive, &mut files);
    files.sort();
    Ok(files)
}

/// Directories that cannot be read are stepped over: one unreadable folder
/// somewhere in a tree should not cancel the whole run.
fn gather(entries: fs::ReadDir, recursive: bool, files: &mut Vec<PathBuf>) {
    for path in entries.filter_map(|entry| entry.ok().map(|e| e.path())) {
        // A symlink pointing back up the tree would otherwise loop forever.
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if recursive && !is_hidden(&path) {
                if let Ok(entries) = fs::read_dir(&path) {
                    gather(entries, recursive, files);
                }
            }
        } else if meta.is_file() && is_video(&path) && !is_squish_output(&path) {
            files.push(path);
        }
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

/// With `-r` a pattern covers the whole tree, so `*.mov` means `**/*.mov`.
pub fn into_recursive(pattern: &str) -> String {
    let normalized = normalize_separators(pattern);
    if normalized.contains("**") {
        return normalized;
    }
    match normalized.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/**/{file}"),
        None => format!("**/{normalized}"),
    }
}

/// The deepest directory holding every input, so `--out-dir` can keep the shape
/// of the tree it was given.
pub fn common_base(inputs: &[PathBuf]) -> Option<PathBuf> {
    let mut base = inputs.first()?.parent()?.to_path_buf();
    for input in inputs.iter().skip(1) {
        let parent = input.parent()?;
        while !parent.starts_with(&base) {
            if !base.pop() {
                return None;
            }
        }
    }
    Some(base)
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
}

/// `clip.mov` next to `out_dir` becomes `clip.squish.mp4`. Under `--out-dir`,
/// whatever of the input's path lies below `base` is recreated there.
pub fn output_path(input: &Path, out_dir: Option<&Path>, base: Option<&Path>) -> Result<PathBuf> {
    let stem = input
        .file_stem()
        .with_context(|| format!("{} has no file name", input.display()))?;
    let mut name = stem.to_os_string();
    name.push(format!(".{OUTPUT_MARKER}.{OUTPUT_EXTENSION}"));

    let dir = match out_dir {
        Some(out) => out.join(below(input, base)),
        None => input.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    Ok(dir.join(name))
}

fn below(input: &Path, base: Option<&Path>) -> PathBuf {
    let Some(parent) = input.parent() else {
        return PathBuf::new();
    };
    base.and_then(|base| parent.strip_prefix(base).ok())
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Outputs of an earlier run, so `squish *.mp4` does not eat its own results.
pub fn is_squish_output(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let is_mp4 = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(OUTPUT_EXTENSION));
    is_mp4
        && Path::new(stem)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(OUTPUT_MARKER))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn as_string(paths: &[PathBuf]) -> Vec<String> {
        let mut names: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn pattern_in(dir: &Path, tail: &str) -> String {
        dir.join(tail).to_string_lossy().into_owned()
    }

    #[test]
    fn output_name_sits_next_to_the_source() {
        let out = output_path(Path::new("clip.mov"), None, None).unwrap();
        assert_eq!(out, PathBuf::from("clip.squish.mp4"));
    }

    #[test]
    fn output_name_keeps_inner_dots_and_the_directory() {
        let out = output_path(Path::new("/videos/my.clip.MOV"), None, None).unwrap();
        assert_eq!(out, PathBuf::from("/videos/my.clip.squish.mp4"));
    }

    #[test]
    fn out_dir_moves_the_output() {
        let out = output_path(
            Path::new("/videos/clip.mov"),
            Some(Path::new("/tmp/enc")),
            None,
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/tmp/enc/clip.squish.mp4"));
    }

    #[test]
    fn out_dir_rebuilds_the_tree_under_the_base() {
        let out = output_path(
            Path::new("/videos/2024/may/clip.mov"),
            Some(Path::new("/tmp/enc")),
            Some(Path::new("/videos")),
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/tmp/enc/2024/may/clip.squish.mp4"));

        let flat = output_path(
            Path::new("/videos/clip.mov"),
            Some(Path::new("/tmp/enc")),
            Some(Path::new("/videos")),
        )
        .unwrap();
        assert_eq!(flat, PathBuf::from("/tmp/enc/clip.squish.mp4"));
    }

    #[test]
    fn a_base_that_does_not_fit_leaves_the_output_flat() {
        let out = output_path(
            Path::new("/elsewhere/clip.mov"),
            Some(Path::new("/tmp/enc")),
            Some(Path::new("/videos")),
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/tmp/enc/clip.squish.mp4"));
    }

    #[test]
    fn the_base_is_the_deepest_shared_directory() {
        let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            common_base(&paths(&["/a/b/one.mov", "/a/b/c/two.mov"])),
            Some(PathBuf::from("/a/b"))
        );
        assert_eq!(
            common_base(&paths(&["/a/b/one.mov", "/a/d/two.mov"])),
            Some(PathBuf::from("/a"))
        );
        assert_eq!(
            common_base(&paths(&["/a/one.mov"])),
            Some(PathBuf::from("/a"))
        );
        assert_eq!(common_base(&[]), None);
    }

    #[test]
    fn a_recursive_pattern_reaches_into_subdirectories() {
        assert_eq!(into_recursive("*.mov"), "**/*.mov");
        assert_eq!(into_recursive("clips/*.mov"), "clips/**/*.mov");
        assert_eq!(into_recursive("clips/**/*.mov"), "clips/**/*.mov");
    }

    #[test]
    fn a_recursive_pattern_matches_here_as_well_as_deeper() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a").join("b")).unwrap();
        fs::write(dir.path().join("top.mov"), b"x").unwrap();
        fs::write(dir.path().join("a").join("mid.mov"), b"x").unwrap();
        fs::write(dir.path().join("a").join("b").join("deep.mov"), b"x").unwrap();

        let pattern = into_recursive(&pattern_in(dir.path(), "*.mov"));
        let files = expand_patterns(&[pattern]).unwrap();
        assert_eq!(as_string(&files), ["deep.mov", "mid.mov", "top.mov"]);
    }

    #[test]
    fn a_bare_directory_can_be_walked_down() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sub").join("deeper")).unwrap();
        fs::create_dir(dir.path().join(".hidden")).unwrap();
        fs::write(dir.path().join("top.mov"), b"x").unwrap();
        fs::write(dir.path().join("sub").join("mid.mp4"), b"x").unwrap();
        fs::write(dir.path().join("sub").join("old.squish.mp4"), b"x").unwrap();
        fs::write(dir.path().join("sub").join("deeper").join("deep.mkv"), b"x").unwrap();
        fs::write(dir.path().join(".hidden").join("secret.mov"), b"x").unwrap();

        assert_eq!(
            as_string(&videos_in(dir.path(), false).unwrap()),
            ["top.mov"]
        );
        assert_eq!(
            as_string(&videos_in(dir.path(), true).unwrap()),
            ["deep.mkv", "mid.mp4", "top.mov"]
        );
    }

    #[test]
    fn recognises_its_own_outputs() {
        assert!(is_squish_output(Path::new("clip.squish.mp4")));
        assert!(is_squish_output(Path::new("/a/b/my.clip.SQUISH.MP4")));
        assert!(!is_squish_output(Path::new("clip.mp4")));
        assert!(!is_squish_output(Path::new("squish.mp4")));
        assert!(!is_squish_output(Path::new("clip.squish.mov")));
    }

    #[test]
    fn a_bare_directory_yields_its_videos() {
        let dir = tempdir().unwrap();
        for name in [
            "a.mov",
            "b.MP4",
            "c.mkv",
            "notes.txt",
            "poster.jpg",
            "old.squish.mp4",
        ] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        fs::create_dir(dir.path().join("sub.mov")).unwrap();

        let files = videos_in(dir.path(), false).unwrap();
        assert_eq!(as_string(&files), ["a.mov", "b.MP4", "c.mkv"]);
    }

    #[test]
    fn glob_expands_and_keeps_only_files() {
        let dir = tempdir().unwrap();
        for name in ["a.mov", "b.mov", "c.mp4"] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        fs::create_dir(dir.path().join("nested.mov")).unwrap();

        let files = expand_patterns(&[pattern_in(dir.path(), "*.mov")]).unwrap();
        assert_eq!(as_string(&files), ["a.mov", "b.mov"]);
    }

    #[test]
    fn overlapping_patterns_are_deduplicated() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.mov"), b"x").unwrap();

        let files = expand_patterns(&[
            pattern_in(dir.path(), "*.mov"),
            pattern_in(dir.path(), "a.*"),
            pattern_in(dir.path(), "a.mov"),
        ])
        .unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn a_plain_existing_path_is_taken_literally() {
        let dir = tempdir().unwrap();
        let odd = dir.path().join("weird[name].mov");
        fs::write(&odd, b"x").unwrap();

        let files = expand_patterns(&[odd.to_string_lossy().into_owned()]).unwrap();
        assert_eq!(files, [odd]);
    }

    #[test]
    fn a_pattern_matching_nothing_yields_nothing() {
        let dir = tempdir().unwrap();
        let files = expand_patterns(&[pattern_in(dir.path(), "*.mkv")]).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn star_does_not_cross_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("deep.mov"), b"x").unwrap();
        fs::write(dir.path().join("top.mov"), b"x").unwrap();

        let files = expand_patterns(&[pattern_in(dir.path(), "*.mov")]).unwrap();
        assert_eq!(as_string(&files), ["top.mov"]);
    }
}
