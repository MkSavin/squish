use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{bail, Context, Result};

use crate::cli::{Backend, Codec, Plan, RateControl};
use crate::probe::Source;
use crate::progress::{self, Tick};
use crate::shutdown;

pub const FFMPEG: &str = "ffmpeg";
pub const FFPROBE: &str = "ffprobe";

const AUDIO_BITRATE: &str = "192k";
const WATCHDOG_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Fails early with a readable message instead of one "not found" per file.
pub fn preflight() -> Result<()> {
    for tool in [FFMPEG, FFPROBE] {
        let status = Command::new(tool)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => bail!("`{tool} -version` exited with {s}; the {tool} in PATH looks broken"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("{tool} was not found in PATH; install ffmpeg and add its bin directory to PATH")
            }
            Err(e) => return Err(e).with_context(|| format!("failed to run {tool}")),
        }
    }
    Ok(())
}

/// Decoder names this ffmpeg build knows, queried once. `None` when the query
/// itself failed, which means "assume every stream is fine".
pub fn decoders() -> Option<&'static HashSet<String>> {
    static DECODERS: OnceLock<Option<HashSet<String>>> = OnceLock::new();
    DECODERS
        .get_or_init(|| {
            let output = Command::new(FFMPEG)
                .args(["-hide_banner", "-decoders"])
                .stdin(Stdio::null())
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| parse_decoders(&String::from_utf8_lossy(&output.stdout)))
        })
        .as_ref()
}

/// Encoder names this ffmpeg build carries, queried once.
pub fn encoders() -> Option<&'static HashSet<String>> {
    static ENCODERS: OnceLock<Option<HashSet<String>>> = OnceLock::new();
    ENCODERS
        .get_or_init(|| {
            let output = Command::new(FFMPEG)
                .args(["-hide_banner", "-encoders"])
                .stdin(Stdio::null())
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| parse_decoders(&String::from_utf8_lossy(&output.stdout)))
        })
        .as_ref()
}

/// Told before any file is touched, rather than as a wall of ffmpeg output.
pub fn check_encoder(plan: &Plan) -> Result<()> {
    let encoder = plan.codec.encoder(plan.backend);
    let Some(known) = encoders() else {
        return Ok(());
    };
    if known.contains(encoder) {
        return Ok(());
    }
    match plan.backend {
        Backend::Cpu => bail!("this ffmpeg build has no {encoder}"),
        _ => bail!("this ffmpeg build has no {encoder}; --cpu encodes without it"),
    }
}

/// Lines look like ` A....D aac  AAC (Advanced Audio Coding)`.
fn parse_decoders(listing: &str) -> HashSet<String> {
    listing
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            let name = parts.next()?;
            let is_flags =
                flags.len() == 6 && flags.chars().all(|c| c == '.' || c.is_ascii_alphabetic());
            // The legend above the table has the same shape, with `=` for a name.
            (is_flags && name.starts_with(|c: char| c.is_ascii_alphanumeric()))
                .then(|| name.to_owned())
        })
        .collect()
}

/// Everything ffmpeg needs for one file, built without touching the disk.
pub fn build_args(input: &Path, output: &Path, plan: &Plan, src: &Source) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::with_capacity(40);
    macro_rules! push {
        ($($value:expr),+ $(,)?) => {{ $( args.push(OsString::from($value)); )+ }};
    }

    push!("-hide_banner", "-y");
    if plan.backend != Backend::Cpu {
        push!("-hwaccel", "auto");
    }
    push!("-i", input);

    push!("-map", "0:v:0");
    if let Some(index) = src.audio_stream {
        push!("-map", format!("0:a:{index}?"));
    }
    push!("-map_chapters", "-1", "-sn", "-dn");

    let ten_bit = plan.ten_bit && src.is_ten_bit();
    push!("-c:v", plan.codec.encoder(plan.backend));
    if let Some(preset) = preset_value(plan) {
        push!("-preset", preset);
    }
    push!("-pix_fmt", pixel_format(plan, ten_bit));
    if ten_bit && plan.codec == Codec::Hevc {
        push!("-profile:v", "main10");
    }

    match (plan.rate, plan.backend) {
        (RateControl::Quality(q), Backend::Nvenc) => {
            push!("-rc", "vbr", "-cq", q.to_string(), "-b:v", "0")
        }
        (RateControl::Quality(q), Backend::VideoToolbox) => {
            push!("-q:v", videotoolbox_quality(q).to_string(), "-b:v", "0")
        }
        (RateControl::Quality(q), Backend::Cpu) => push!("-crf", q.to_string()),
        (RateControl::Bitrate(bits), Backend::Nvenc) => push!(
            "-rc",
            "vbr",
            "-b:v",
            bits.to_string(),
            "-maxrate",
            bits.saturating_mul(2).to_string()
        ),
        (RateControl::Bitrate(bits), _) => push!("-b:v", bits.to_string()),
    }

    if let Some(tag) = plan.codec.mp4_tag() {
        push!("-tag:v", tag);
    }

    if src.audio_stream.is_some() {
        push!("-c:a", "aac", "-b:a", AUDIO_BITRATE);
    }

    if plan.keep_metadata {
        push!(
            "-map_metadata",
            "0",
            "-movflags",
            "+faststart+use_metadata_tags"
        );
        if let Some(time) = &src.creation_time {
            push!("-metadata", format!("creation_time={time}"));
        }
    } else {
        push!("-map_metadata", "-1", "-movflags", "+faststart");
    }

    push!(output);
    args
}

/// Machine-readable progress on stdout instead of the usual status line.
pub fn with_progress(args: &[OsString]) -> Vec<OsString> {
    let mut out = Vec::with_capacity(args.len() + 5);
    out.push(OsString::from("-hide_banner"));
    for extra in ["-nostats", "-loglevel", "error", "-progress", "pipe:1"] {
        out.push(OsString::from(extra));
    }
    out.extend(
        args.iter()
            .skip_while(|a| *a == "-hide_banner")
            .map(ToOwned::to_owned),
    );
    out
}

fn preset_value(plan: &Plan) -> Option<String> {
    match (plan.backend, plan.codec) {
        (Backend::Nvenc, _) => Some(plan.preset.nvenc()),
        (Backend::VideoToolbox, _) => None,
        (Backend::Cpu, Codec::Av1) => Some(plan.preset.svtav1().to_owned()),
        (Backend::Cpu, _) => Some(plan.preset.x26x().to_owned()),
    }
}

/// VideoToolbox rates quality 1-100 upwards, the opposite of a 0-51 CQ.
fn videotoolbox_quality(cq: u8) -> u8 {
    let scaled = (51u32.saturating_sub(cq as u32) * 100) / 51;
    scaled.clamp(1, 100) as u8
}

/// NVENC h264 only accepts 8-bit 4:2:0; feeding it yuv420p10le makes ffmpeg
/// claim there is no NVENC device at all.
fn pixel_format(plan: &Plan, ten_bit: bool) -> &'static str {
    match (ten_bit, plan.backend) {
        (false, _) => "yuv420p",
        (true, Backend::Cpu) => "yuv420p10le",
        (true, _) => "p010le",
    }
}

/// Quoted so the line can be pasted back into a shell.
pub fn display_command(args: &[OsString]) -> String {
    let mut line = String::from(FFMPEG);
    for arg in args {
        let text = arg.to_string_lossy();
        line.push(' ');
        if text.is_empty() || text.contains(' ') {
            line.push('"');
            line.push_str(&text);
            line.push('"');
        } else {
            line.push_str(&text);
        }
    }
    line
}

/// Whether ffmpeg ran to the end or was stopped on the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Finished,
    Interrupted,
}

/// Runs ffmpeg with inherited stdio so its own progress output reaches the terminal.
pub fn run(args: &[OsString]) -> Result<Completion> {
    let status = Command::new(FFMPEG)
        .args(args.iter().map(OsStr::new))
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("failed to run {FFMPEG}"))?;
    if shutdown::requested() {
        return Ok(Completion::Interrupted);
    }
    check(status.code(), status.success(), None)?;
    Ok(Completion::Finished)
}

/// Runs ffmpeg quietly and reports progress, for when several encodes share the terminal.
pub fn run_watched(args: &[OsString], mut on_tick: impl FnMut(Tick)) -> Result<Completion> {
    let mut child = Command::new(FFMPEG)
        .args(args.iter().map(OsStr::new))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {FFMPEG}"))?;

    let mut errors = child.stderr.take().expect("stderr is piped");
    let collector = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = errors.read_to_string(&mut text);
        text
    });

    let stdout = child.stdout.take().expect("stdout is piped");
    // A slow encode can go seconds between progress lines, so waiting for the
    // read loop to notice Ctrl+C is not good enough: watch for it separately.
    let child = Arc::new(Mutex::new(child));
    let watchdog = {
        let child = Arc::clone(&child);
        std::thread::spawn(move || loop {
            if shutdown::requested() {
                let _ = child.lock().expect("ffmpeg handle").kill();
                return;
            }
            match child.lock().expect("ffmpeg handle").try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(WATCHDOG_POLL),
            }
        })
    };

    let mut state = progress::Tick::default();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if progress::apply(&line, &mut state) {
            on_tick(state);
        }
    }

    let status = child.lock().expect("ffmpeg handle").wait();
    let status = status.context("waiting for ffmpeg")?;
    let _ = watchdog.join();
    let stderr = collector.join().unwrap_or_default();
    if shutdown::requested() {
        return Ok(Completion::Interrupted);
    }
    check(status.code(), status.success(), Some(&stderr))?;
    Ok(Completion::Finished)
}

/// Said when every hardware encoder session is taken; the file is worth retrying.
pub const BUSY: &str = "no free encoder session on the GPU";

/// ffmpeg states these as driver internals, which says nothing about what to do.
fn hint(stderr: &str) -> Option<&'static str> {
    const PATTERNS: [(&str, &str); 5] = [
        ("openencodesessionex failed", BUSY),
        ("incompatible client key", BUSY),
        (
            "does not support the required nvenc api",
            "the GPU driver is older than this ffmpeg build expects; update it or use --cpu",
        ),
        (
            "cannot load nvencodeapi",
            "no NVENC driver was found; --cpu encodes without a GPU",
        ),
        (
            "no nvenc capable devices found",
            "NVENC refused the input; this is usually an unsupported pixel format or a busy GPU",
        ),
    ];
    let lowered = stderr.to_lowercase();
    PATTERNS
        .iter()
        .find(|(pattern, _)| lowered.contains(pattern))
        .map(|(_, hint)| *hint)
}

/// Whether the same file is worth another go in a moment.
pub fn is_busy(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains(BUSY)
}

fn check(code: Option<i32>, success: bool, stderr: Option<&str>) -> Result<()> {
    if success {
        return Ok(());
    }
    let reason = match code {
        Some(code) => format!("ffmpeg exited with code {code}"),
        None => "ffmpeg was terminated by a signal".to_owned(),
    };
    let Some(text) = stderr.map(str::trim).filter(|s| !s.is_empty()) else {
        bail!("{reason}");
    };
    match hint(text) {
        Some(hint) => bail!("{hint} ({})", last_lines(text, 2)),
        None => bail!("{reason}: {}", last_lines(text, 3)),
    }
}

fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(count)..].join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Preset, Profile};
    use std::path::PathBuf;

    fn config() -> Plan {
        Plan {
            codec: Codec::H264,
            backend: Backend::Nvenc,
            rate: RateControl::Quality(24),
            preset: Preset(5),
            ten_bit: false,
            keep_metadata: false,
            profile: Profile::Web,
        }
    }

    fn eight_bit() -> Source {
        Source {
            pix_fmt: Some("yuv420p".into()),
            bit_depth: Some(8),
            duration: Some(10.0),
            audio_stream: Some(0),
            ..Source::default()
        }
    }

    fn ten_bit() -> Source {
        Source {
            pix_fmt: Some("yuv420p10le".into()),
            bit_depth: Some(10),
            creation_time: Some("2024-05-01T10:00:00.000000Z".into()),
            duration: Some(10.0),
            audio_stream: Some(0),
            ..Source::default()
        }
    }

    fn args_of(cfg: &Plan, src: &Source) -> Vec<String> {
        build_args(
            &PathBuf::from("in.mov"),
            &PathBuf::from("in.squish.mp4"),
            cfg,
            src,
        )
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
    }

    fn value_after(args: &[String], flag: &str) -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .map(|i| args[i + 1].clone())
    }

    fn contains_pair(args: &[String], flag: &str, value: &str) -> bool {
        value_after(args, flag).as_deref() == Some(value)
    }

    #[test]
    fn default_is_nvenc_constant_quality() {
        let args = args_of(&config(), &eight_bit());
        assert!(contains_pair(&args, "-c:v", "h264_nvenc"));
        assert!(contains_pair(&args, "-rc", "vbr"));
        assert!(contains_pair(&args, "-cq", "24"));
        assert!(contains_pair(&args, "-b:v", "0"));
        assert!(contains_pair(&args, "-preset", "p5"));
        assert!(!args.iter().any(|a| a == "-maxrate"));
    }

    #[test]
    fn input_comes_before_output_options() {
        let args = args_of(&config(), &eight_bit());
        let input = args.iter().position(|a| a == "-i").unwrap();
        let codec = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[input + 1], "in.mov");
        assert!(input < codec);
        assert_eq!(args.last().unwrap(), "in.squish.mp4");
    }

    #[test]
    fn eight_bit_pixel_format_is_forced_even_for_ten_bit_sources() {
        let args = args_of(&config(), &ten_bit());
        assert!(contains_pair(&args, "-pix_fmt", "yuv420p"));
        assert!(!args.iter().any(|a| a == "-profile:v"));
    }

    #[test]
    fn ten_bit_hevc_uses_p010_and_main10() {
        let cfg = Plan {
            codec: Codec::Hevc,
            ten_bit: true,
            ..config()
        };
        let args = args_of(&cfg, &ten_bit());
        assert!(contains_pair(&args, "-c:v", "hevc_nvenc"));
        assert!(contains_pair(&args, "-pix_fmt", "p010le"));
        assert!(contains_pair(&args, "-profile:v", "main10"));
        assert!(contains_pair(&args, "-tag:v", "hvc1"));
    }

    #[test]
    fn ten_bit_request_on_an_eight_bit_source_stays_eight_bit() {
        let cfg = Plan {
            codec: Codec::Hevc,
            ten_bit: true,
            ..config()
        };
        let args = args_of(&cfg, &eight_bit());
        assert!(contains_pair(&args, "-pix_fmt", "yuv420p"));
        assert!(!args.iter().any(|a| a == "-profile:v"));
    }

    #[test]
    fn av1_is_tagged_for_quicktime() {
        let cfg = Plan {
            codec: Codec::Av1,
            ..config()
        };
        let args = args_of(&cfg, &eight_bit());
        assert!(contains_pair(&args, "-c:v", "av1_nvenc"));
        assert!(contains_pair(&args, "-tag:v", "av01"));
    }

    #[test]
    fn h264_gets_no_tag() {
        let args = args_of(&config(), &eight_bit());
        assert!(!args.iter().any(|a| a == "-tag:v"));
    }

    #[test]
    fn bitrate_mode_replaces_constant_quality() {
        let cfg = Plan {
            rate: RateControl::Bitrate(8_000_000),
            ..config()
        };
        let args = args_of(&cfg, &eight_bit());
        assert!(contains_pair(&args, "-b:v", "8000000"));
        assert!(contains_pair(&args, "-maxrate", "16000000"));
        assert!(!args.iter().any(|a| a == "-cq"));
    }

    #[test]
    fn cpu_mode_uses_crf_and_named_presets() {
        let cfg = Plan {
            backend: Backend::Cpu,
            ..config()
        };
        let args = args_of(&cfg, &eight_bit());
        assert!(contains_pair(&args, "-c:v", "libx264"));
        assert!(contains_pair(&args, "-crf", "24"));
        assert!(contains_pair(&args, "-preset", "medium"));
        assert!(!args.iter().any(|a| a == "-rc"));
        assert!(!args.iter().any(|a| a == "-hwaccel"));
    }

    #[test]
    fn cpu_ten_bit_uses_planar_ten_bit() {
        let cfg = Plan {
            codec: Codec::Hevc,
            backend: Backend::Cpu,
            ten_bit: true,
            ..config()
        };
        let args = args_of(&cfg, &ten_bit());
        assert!(contains_pair(&args, "-c:v", "libx265"));
        assert!(contains_pair(&args, "-pix_fmt", "yuv420p10le"));
    }

    #[test]
    fn cpu_av1_preset_is_numeric() {
        let cfg = Plan {
            codec: Codec::Av1,
            backend: Backend::Cpu,
            preset: Preset(7),
            ..config()
        };
        let args = args_of(&cfg, &eight_bit());
        assert!(contains_pair(&args, "-c:v", "libsvtav1"));
        assert!(contains_pair(&args, "-preset", "2"));
    }

    #[test]
    fn videotoolbox_uses_its_own_quality_scale_and_no_preset() {
        let cfg = Plan {
            codec: Codec::Hevc,
            backend: Backend::VideoToolbox,
            ten_bit: true,
            ..config()
        };
        let args = args_of(&cfg, &ten_bit());
        assert!(contains_pair(&args, "-c:v", "hevc_videotoolbox"));
        assert!(contains_pair(&args, "-pix_fmt", "p010le"));
        assert!(contains_pair(&args, "-profile:v", "main10"));
        assert!(contains_pair(&args, "-q:v", "52"));
        assert!(!args.iter().any(|a| a == "-preset"));
        assert!(!args.iter().any(|a| a == "-rc"));
    }

    #[test]
    fn videotoolbox_quality_runs_the_other_way() {
        assert_eq!(videotoolbox_quality(0), 100);
        assert_eq!(videotoolbox_quality(24), 52);
        assert_eq!(videotoolbox_quality(51), 1);
    }

    #[test]
    fn audio_is_mapped_and_re_encoded() {
        let args = args_of(&config(), &eight_bit());
        assert!(contains_pair(&args, "-map", "0:v:0"));
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "0:a:0?"));
        assert!(contains_pair(&args, "-c:a", "aac"));
        assert!(contains_pair(&args, "-b:a", "192k"));
    }

    #[test]
    fn a_later_audio_stream_is_mapped_by_index() {
        let src = Source {
            audio_stream: Some(1),
            ..eight_bit()
        };
        let args = args_of(&config(), &src);
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "0:a:1?"));
        assert!(contains_pair(&args, "-c:a", "aac"));
    }

    #[test]
    fn a_file_without_usable_audio_gets_no_audio_options() {
        let src = Source {
            audio_stream: None,
            ..eight_bit()
        };
        let args = args_of(&config(), &src);
        assert!(!args.iter().any(|a| a.starts_with("0:a")));
        assert!(!args.iter().any(|a| a == "-c:a"));
        assert!(!args.iter().any(|a| a == "-b:a"));
    }

    #[test]
    fn subtitles_chapters_and_data_are_dropped() {
        let args = args_of(&config(), &eight_bit());
        assert!(args.iter().any(|a| a == "-sn"));
        assert!(args.iter().any(|a| a == "-dn"));
        assert!(contains_pair(&args, "-map_chapters", "-1"));
    }

    #[test]
    fn metadata_is_dropped_by_default() {
        let args = args_of(&config(), &ten_bit());
        assert!(contains_pair(&args, "-map_metadata", "-1"));
        assert!(!args.iter().any(|a| a == "-metadata"));
    }

    #[test]
    fn keep_metadata_carries_the_capture_time() {
        let cfg = Plan {
            keep_metadata: true,
            ..config()
        };
        let args = args_of(&cfg, &ten_bit());
        assert!(contains_pair(&args, "-map_metadata", "0"));
        assert!(contains_pair(
            &args,
            "-metadata",
            "creation_time=2024-05-01T10:00:00.000000Z"
        ));
        assert!(contains_pair(
            &args,
            "-movflags",
            "+faststart+use_metadata_tags"
        ));
    }

    #[test]
    fn keep_metadata_without_a_timestamp_adds_no_tag() {
        let cfg = Plan {
            keep_metadata: true,
            ..config()
        };
        let args = args_of(&cfg, &eight_bit());
        assert!(!args.iter().any(|a| a == "-metadata"));
    }

    #[test]
    fn progress_flags_go_before_the_input() {
        let plain = build_args(
            Path::new("in.mov"),
            Path::new("out.mp4"),
            &config(),
            &eight_bit(),
        );
        let watched: Vec<String> = with_progress(&plain)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let input = watched.iter().position(|a| a == "-i").unwrap();
        let progress = watched.iter().position(|a| a == "-progress").unwrap();
        assert!(progress < input);
        assert!(contains_pair(&watched, "-progress", "pipe:1"));
        assert!(watched.iter().any(|a| a == "-nostats"));
        assert_eq!(watched.iter().filter(|a| *a == "-hide_banner").count(), 1);
        assert_eq!(watched.last().unwrap(), "out.mp4");
    }

    #[test]
    fn command_line_quotes_paths_with_spaces() {
        let args = build_args(
            &PathBuf::from("my clip.mov"),
            &PathBuf::from("my clip.squish.mp4"),
            &config(),
            &eight_bit(),
        );
        let line = display_command(&args);
        assert!(line.starts_with("ffmpeg "));
        assert!(line.contains("-i \"my clip.mov\""));
        assert!(line.ends_with("\"my clip.squish.mp4\""));
    }

    #[test]
    fn decoder_listing_is_parsed() {
        let listing = " V..... = Video\n ------\n V....D h264   H.264\n A....D aac    AAC\n A..... apac   Apple Positional Audio Codec\n";
        let decoders = parse_decoders(listing);
        assert!(decoders.contains("h264"));
        assert!(decoders.contains("aac"));
        assert!(decoders.contains("apac"));
        assert!(!decoders.contains("="));
    }

    #[test]
    fn failures_carry_the_tail_of_stderr() {
        let error = check(Some(1), false, Some("one\n\ntwo\nthree\nfour\n")).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("code 1"));
        assert!(text.contains("two; three; four"));
        assert!(!text.contains("one;"));
    }

    #[test]
    fn driver_complaints_are_translated() {
        let cases = [
            (
                "[hevc_nvenc @ 0x1] Driver does not support the required nvenc API version",
                "update it",
            ),
            (
                "[h264_nvenc @ 0x1] OpenEncodeSessionEx failed: out of memory (10)",
                BUSY,
            ),
            ("Cannot load nvEncodeAPI64.dll", "--cpu"),
            ("[h264_nvenc @ 0x1] No NVENC capable devices found", "NVENC"),
        ];
        for (stderr, expected) in cases {
            let text = check(Some(1), false, Some(stderr)).unwrap_err().to_string();
            assert!(text.contains(expected), "{stderr} gave {text}");
        }
    }

    #[test]
    fn a_taken_encoder_session_is_worth_retrying() {
        let busy = check(Some(1), false, Some("OpenEncodeSessionEx failed")).unwrap_err();
        assert!(is_busy(&busy.context("encoding clip.mov")));

        let broken = check(Some(1), false, Some("moov atom not found")).unwrap_err();
        assert!(!is_busy(&broken));
    }
}
