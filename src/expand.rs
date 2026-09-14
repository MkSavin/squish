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
pub fn videos_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file() && is_video(path) && !is_squish_output(path))
        .collect();
    files.sort();
    Ok(files)
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
}

/// `clip.mov` next to `out_dir` becomes `clip.squish.mp4`.
pub fn output_path(input: &Path, out_dir: Option<&Path>) -> Result<PathBuf> {
    let stem = input
        .file_stem()
        .with_context(|| format!("{} has no file name", input.display()))?;
    let mut name = stem.to_os_string();
    name.push(format!(".{OUTPUT_MARKER}.{OUTPUT_EXTENSION}"));

    let dir = out_dir
        .map(Path::to_path_buf)
        .or_else(|| input.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    Ok(dir.join(name))
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
        let out = output_path(Path::new("clip.mov"), None).unwrap();
        assert_eq!(out, PathBuf::from("clip.squish.mp4"));
    }

    #[test]
    fn output_name_keeps_inner_dots_and_the_directory() {
        let out = output_path(Path::new("/videos/my.clip.MOV"), None).unwrap();
        assert_eq!(out, PathBuf::from("/videos/my.clip.squish.mp4"));
    }

    #[test]
    fn out_dir_moves_the_output() {
        let out = output_path(Path::new("/videos/clip.mov"), Some(Path::new("/tmp/enc"))).unwrap();
        assert_eq!(out, PathBuf::from("/tmp/enc/clip.squish.mp4"));
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

        let files = videos_in(dir.path()).unwrap();
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
