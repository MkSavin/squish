//! End-to-end runs against a real ffmpeg. Skipped where ffmpeg is missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::{tempdir, TempDir};

fn ffmpeg_missing() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
}

/// Skips the test instead of failing it, so the suite still runs without ffmpeg.
macro_rules! needs_ffmpeg {
    () => {
        if ffmpeg_missing() {
            eprintln!("no ffmpeg in PATH, skipping");
            return;
        }
    };
}

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            dir: tempdir().expect("a temporary directory"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A deliberately fat clip, so nothing is skipped for being small already.
    fn video(&self, name: &str, seconds: u32, audio: bool) -> PathBuf {
        let path = self.path(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("the parent directory");
        }
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg.args(["-v", "error", "-y", "-f", "lavfi", "-i"]);
        ffmpeg.arg("testsrc2=size=320x240:rate=30");
        if audio {
            ffmpeg.args(["-f", "lavfi", "-i", "sine=frequency=440"]);
        }
        ffmpeg.args(["-t", &seconds.to_string()]);
        ffmpeg.args(["-c:v", "libx264", "-preset", "ultrafast", "-b:v", "4M"]);
        if audio {
            ffmpeg.args(["-c:a", "aac", "-shortest"]);
        }
        ffmpeg.args(["-movflags", "+faststart"]).arg(&path);
        let status = ffmpeg.status().expect("ffmpeg runs");
        assert!(status.success(), "could not build {name}");
        path
    }

    fn squish(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_squish"))
            .args(args)
            .current_dir(self.dir.path())
            .stdin(Stdio::null())
            .output()
            .expect("squish runs")
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn duration_of(path: &Path) -> f64 {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("a duration")
}

#[test]
fn a_bare_run_encodes_the_folder_and_keeps_the_originals() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    fixture.video("one.mp4", 1, true);
    fixture.video("two.mp4", 1, false);

    let run = fixture.squish(&["--cpu", "--preset", "p1"]);
    assert!(run.status.success(), "{}", stdout_of(&run));

    let text = stdout_of(&run);
    assert!(text.contains("2 encoded, 0 skipped, 0 failed"), "{text}");
    for name in ["one", "two"] {
        assert!(fixture.path(&format!("{name}.mp4")).is_file());
        let encoded = fixture.path(&format!("{name}.squish.mp4"));
        assert!(encoded.is_file(), "{name} produced nothing");
        assert!((duration_of(&encoded) - 1.0).abs() < 0.2);
    }
}

#[test]
fn the_output_inherits_the_timestamp_of_its_source() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    let source = fixture.video("clip.mp4", 1, false);

    assert!(fixture
        .squish(&["--cpu", "--preset", "p1"])
        .status
        .success());

    let when = |path: &Path| fs::metadata(path).unwrap().modified().unwrap();
    assert_eq!(when(&source), when(&fixture.path("clip.squish.mp4")));
}

#[test]
fn a_source_that_stops_early_is_a_failure_not_a_success() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    let whole = fixture.video("whole.mp4", 8, false);

    // faststart puts the header first, so the cut file still claims 8 seconds
    // while holding a fraction of the frames.
    let bytes = fs::read(&whole).expect("the clip");
    let cut = fixture.path("cut.mp4");
    fs::write(&cut, &bytes[..bytes.len() * 35 / 100]).expect("the cut clip");
    fs::remove_file(&whole).expect("the whole clip goes");

    let run = fixture.squish(&["--cpu", "--preset", "p1", "-j", "1"]);
    assert!(!run.status.success(), "a short output must not pass");

    let text = stdout_of(&run);
    assert!(text.contains("0 encoded"), "{text}");
    assert!(text.contains("the source 8.0s"), "{text}");
    assert!(
        !fixture.path("cut.squish.mp4").exists(),
        "the short output must be removed"
    );

    let waived = fixture.squish(&["--cpu", "--preset", "p1", "-j", "1", "--no-verify"]);
    assert!(waived.status.success());
    assert!(fixture.path("cut.squish.mp4").is_file());
}

#[test]
fn a_file_that_is_already_light_is_left_alone() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    let lean = fixture.path("lean.mp4");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=640x480:rate=30")
        .args(["-t", "2", "-c:v", "libx264", "-preset", "ultrafast"])
        .args(["-b:v", "150k"])
        .arg(&lean)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success());

    let run = fixture.squish(&["--cpu", "--preset", "p1"]);
    let text = stdout_of(&run);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("already h264 at"), "{text}");
    assert!(!fixture.path("lean.squish.mp4").exists());

    let forced = fixture.squish(&["--cpu", "--preset", "p1", "--force"]);
    assert!(forced.status.success());
    assert!(fixture.path("lean.squish.mp4").is_file());
}

#[test]
fn recursion_finds_the_tree_and_out_dir_rebuilds_it() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    fixture.video("top.mp4", 1, false);
    fixture.video("a/mid.mp4", 1, false);
    fixture.video("a/b/deep.mp4", 1, false);

    let flat = fixture.squish(&["--cpu", "--preset", "p1"]);
    assert!(
        stdout_of(&flat).contains("1 encoded"),
        "{}",
        stdout_of(&flat)
    );

    let run = fixture.squish(&[
        "-r",
        "--out-dir",
        "enc",
        "--cpu",
        "--preset",
        "p1",
        "--force",
    ]);
    assert!(run.status.success(), "{}", stdout_of(&run));
    assert!(stdout_of(&run).contains("3 encoded"), "{}", stdout_of(&run));

    for relative in ["top.squish.mp4", "a/mid.squish.mp4", "a/b/deep.squish.mp4"] {
        assert!(
            fixture.path(&format!("enc/{relative}")).is_file(),
            "{relative} is not where the tree says"
        );
    }
}

#[test]
fn a_broken_file_fails_alone() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    fixture.video("good.mp4", 1, false);
    fs::write(fixture.path("bad.mp4"), b"not a video at all").expect("the broken file");

    let run = fixture.squish(&["--cpu", "--preset", "p1"]);
    assert!(
        !run.status.success(),
        "a failure must show in the exit code"
    );

    let text = stdout_of(&run);
    assert!(text.contains("1 encoded"), "{text}");
    assert!(text.contains("1 failed"), "{text}");
    assert!(fixture.path("good.squish.mp4").is_file());
    assert!(!fixture.path("bad.squish.mp4").exists());
}

#[test]
fn a_dry_run_writes_nothing_and_quiet_prints_only_paths() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    fixture.video("clip.mp4", 1, false);

    let dry = fixture.squish(&["--cpu", "--dry-run"]);
    assert!(dry.status.success());
    assert!(stdout_of(&dry).contains("-c:v libx264"));
    assert!(!fixture.path("clip.squish.mp4").exists());

    let quiet = fixture.squish(&["--cpu", "--preset", "p1", "--quiet"]);
    assert!(quiet.status.success());
    let printed = stdout_of(&quiet);
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines.len(), 1, "{printed}");
    assert!(lines[0].ends_with("clip.squish.mp4"), "{printed}");
}

#[test]
fn originals_go_to_the_trash_only_when_asked() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    let source = fixture.video("clip.mp4", 1, false);

    let kept = fixture.squish(&["--cpu", "--preset", "p1", "--dry-run", "--trash-originals"]);
    assert!(kept.status.success());
    assert!(source.is_file(), "a dry run must trash nothing");
}

/// The macOS hardware path, which has no runner anywhere else.
#[cfg(target_os = "macos")]
#[test]
fn videotoolbox_encodes_on_the_platform_default() {
    needs_ffmpeg!();
    let fixture = Fixture::new();
    fixture.video("clip.mp4", 1, true);

    let run = fixture.squish(&["--preset", "p1"]);
    let text = stdout_of(&run);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("1 encoded"), "{text}");

    let encoded = fixture.path("clip.squish.mp4");
    assert!(encoded.is_file());
    assert!((duration_of(&encoded) - 1.0).abs() < 0.2);
}
