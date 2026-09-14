use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::ffmpeg::{self, FFPROBE};

/// What the encoder needs to know about the input file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Source {
    pub video_codec: Option<String>,
    pub pix_fmt: Option<String>,
    pub bit_depth: Option<u32>,
    pub creation_time: Option<String>,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub bit_rate: Option<u64>,
    /// Index among the audio streams, `None` when there is nothing usable.
    pub audio_stream: Option<usize>,
    pub dropped_audio: Vec<String>,
    /// Written by a phone camera app.
    pub from_camera: bool,
    /// Looks like a screen recording: 4:4:4 or RGB, which cameras never produce.
    pub screen_like: bool,
}

impl Source {
    pub fn is_ten_bit(&self) -> bool {
        self.bit_depth.is_some_and(|d| d >= 10)
    }

    /// Bits per pixel per frame: how heavy the file is, independent of its size
    /// and frame rate. `bytes` stands in when the container states no bitrate.
    pub fn bits_per_pixel(&self, bytes: u64) -> Option<f64> {
        let pixels = f64::from(self.width?) * f64::from(self.height?) * self.fps?;
        let bits = match self.bit_rate {
            Some(rate) => rate as f64,
            None => bytes as f64 * 8.0 / self.duration?,
        };
        (pixels > 0.0 && bits.is_finite()).then(|| bits / pixels)
    }
}

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: Option<ProbeFormat>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    pix_fmt: Option<String>,
    bits_per_raw_sample: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: HashMap<String, String>,
}

pub fn probe(input: &Path) -> Result<Source> {
    let output = Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(input)
        .output()
        .with_context(|| format!("failed to run {FFPROBE}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffprobe failed: {}", stderr.trim());
    }

    let parsed: ProbeOutput =
        serde_json::from_slice(&output.stdout).context("ffprobe returned unreadable json")?;
    parse_probe(parsed, ffmpeg::decoders())
}

/// ffmpeg can exit cleanly on a file that is short or silent, so the result is
/// read back and measured against the source before anything trusts it.
pub fn verify(output: &Path, source: &Source) -> Result<()> {
    let written = probe(output).context("the output cannot be read back")?;
    match compare(source, &written) {
        Some(problem) => bail!("{problem}"),
        None => Ok(()),
    }
}

const DURATION_TOLERANCE: f64 = 0.01;
const DURATION_SLACK: f64 = 0.5;

fn compare(source: &Source, output: &Source) -> Option<String> {
    if source.audio_stream.is_some() && output.audio_stream.is_none() {
        return Some("the output has no audio, the source had some".into());
    }
    let expected = source.duration?;
    let Some(written) = output.duration else {
        return Some("the output states no duration".into());
    };
    let allowed = (expected * DURATION_TOLERANCE).max(DURATION_SLACK);
    ((written - expected).abs() > allowed)
        .then(|| format!("the output is {written:.1}s long, the source {expected:.1}s"))
}

fn parse_probe(parsed: ProbeOutput, decoders: Option<&HashSet<String>>) -> Result<Source> {
    let video = parsed
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .context("file has no video stream")?;

    let pix_fmt = video.pix_fmt.clone();
    let bit_depth = video
        .bits_per_raw_sample
        .as_deref()
        .and_then(|d| d.parse().ok())
        .or_else(|| pix_fmt.as_deref().and_then(bit_depth_of));

    let audio: Vec<String> = parsed
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .map(|s| s.codec_name.clone().unwrap_or_default())
        .collect();
    let audio_stream = pick_audio_stream(&audio, decoders);
    let tags = parsed.format.as_ref().map(|f| &f.tags);

    Ok(Source {
        video_codec: video.codec_name.clone(),
        screen_like: pix_fmt.as_deref().is_some_and(looks_like_a_screen),
        pix_fmt,
        bit_depth,
        width: video.width,
        height: video.height,
        fps: video
            .avg_frame_rate
            .as_deref()
            .and_then(parse_frame_rate)
            .or_else(|| video.r_frame_rate.as_deref().and_then(parse_frame_rate)),
        bit_rate: video
            .bit_rate
            .as_deref()
            .or_else(|| parsed.format.as_ref().and_then(|f| f.bit_rate.as_deref()))
            .and_then(|r| r.parse().ok())
            .filter(|r: &u64| *r > 0),
        duration: parsed
            .format
            .as_ref()
            .and_then(|f| f.duration.as_deref())
            .and_then(|d| d.parse().ok())
            .filter(|d: &f64| d.is_finite() && *d > 0.0),
        creation_time: tags.and_then(|t| t.get("creation_time").cloned()),
        from_camera: tags.is_some_and(written_by_a_phone),
        dropped_audio: audio
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != audio_stream)
            .map(|(_, codec)| codec.clone())
            .collect(),
        audio_stream,
    })
}

/// iPhone clips carry a spatial `apple_apac` track next to the plain aac one,
/// and ffmpeg has no decoder for it, so take the first track it can actually read.
fn pick_audio_stream(audio: &[String], decoders: Option<&HashSet<String>>) -> Option<usize> {
    match decoders {
        Some(known) => audio.iter().position(|codec| known.contains(codec)),
        None => (!audio.is_empty()).then_some(0),
    }
}

/// Camera apps stamp their own namespace into the container tags.
fn written_by_a_phone(tags: &HashMap<String, String>) -> bool {
    tags.keys().any(|key| {
        let key = key.to_ascii_lowercase();
        key.starts_with("com.apple.quicktime.") || key.starts_with("com.android.")
    })
}

/// ffprobe writes frame rates as a fraction, e.g. `30000/1001`.
fn parse_frame_rate(raw: &str) -> Option<f64> {
    let (numerator, denominator) = match raw.split_once('/') {
        Some((n, d)) => (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?),
        None => (raw.parse::<f64>().ok()?, 1.0),
    };
    let fps = numerator / denominator;
    (fps.is_finite() && fps > 0.0).then_some(fps)
}

fn looks_like_a_screen(pix_fmt: &str) -> bool {
    pix_fmt.contains("444")
        || ["rgb", "bgr", "gbr", "argb", "abgr"]
            .iter()
            .any(|prefix| pix_fmt.starts_with(prefix))
}

/// Reads the depth out of names like `yuv420p10le`, `p010le`, `yuv444p12le`.
fn bit_depth_of(pix_fmt: &str) -> Option<u32> {
    let name = pix_fmt.trim_end_matches("le").trim_end_matches("be");
    if let Some(rest) = name.strip_prefix('p') {
        // p010/p210: a chroma layout digit, then the depth.
        if rest.len() >= 3 && rest.chars().all(|c| c.is_ascii_digit()) {
            return rest[1..].parse().ok();
        }
    }
    match name.rsplit_once('p') {
        Some((_, "")) | None => Some(8),
        Some((_, digits)) => digits.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_json(json: &str) -> Result<Source> {
        let known: HashSet<String> = ["h264", "hevc", "aac", "pcm_s16le"]
            .iter()
            .map(|d| (*d).to_owned())
            .collect();
        parse_probe(serde_json::from_str(json).unwrap(), Some(&known))
    }

    #[test]
    fn depth_from_pix_fmt_names() {
        assert_eq!(bit_depth_of("yuv420p"), Some(8));
        assert_eq!(bit_depth_of("yuvj420p"), Some(8));
        assert_eq!(bit_depth_of("nv12"), Some(8));
        assert_eq!(bit_depth_of("rgb24"), Some(8));
        assert_eq!(bit_depth_of("yuv420p10le"), Some(10));
        assert_eq!(bit_depth_of("yuv422p10le"), Some(10));
        assert_eq!(bit_depth_of("p010le"), Some(10));
        assert_eq!(bit_depth_of("p210le"), Some(10));
        assert_eq!(bit_depth_of("yuv444p12le"), Some(12));
    }

    #[test]
    fn reads_streams_and_tags() {
        let source = from_json(
            r#"{
                "streams": [
                    {"codec_type": "video", "codec_name": "hevc", "pix_fmt": "yuv420p10le", "bits_per_raw_sample": "10"},
                    {"codec_type": "audio", "codec_name": "aac"}
                ],
                "format": {"duration": "12.5", "tags": {"creation_time": "2024-05-01T10:00:00.000000Z"}}
            }"#,
        )
        .unwrap();
        assert_eq!(source.video_codec.as_deref(), Some("hevc"));
        assert_eq!(source.pix_fmt.as_deref(), Some("yuv420p10le"));
        assert_eq!(source.bit_depth, Some(10));
        assert!(source.is_ten_bit());
        assert_eq!(
            source.creation_time.as_deref(),
            Some("2024-05-01T10:00:00.000000Z")
        );
        assert_eq!(source.audio_stream, Some(0));
        assert_eq!(source.duration, Some(12.5));
        assert!(source.dropped_audio.is_empty());
        assert!(!source.from_camera);
        assert!(!source.screen_like);
    }

    #[test]
    fn an_undecodable_audio_track_is_skipped() {
        let source = from_json(
            r#"{"streams": [
                {"codec_type": "video", "codec_name": "hevc", "pix_fmt": "yuv420p10le"},
                {"codec_type": "audio", "codec_name": "apple_apac"},
                {"codec_type": "audio", "codec_name": "aac"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(source.audio_stream, Some(1));
        assert_eq!(source.dropped_audio, ["apple_apac"]);
    }

    #[test]
    fn a_file_with_only_undecodable_audio_loses_it() {
        let source = from_json(
            r#"{"streams": [
                {"codec_type": "video", "codec_name": "hevc", "pix_fmt": "yuv420p"},
                {"codec_type": "audio", "codec_name": "apple_apac"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(source.audio_stream, None);
        assert_eq!(source.dropped_audio, ["apple_apac"]);
    }

    #[test]
    fn without_a_decoder_listing_the_first_track_wins() {
        assert_eq!(pick_audio_stream(&["apple_apac".into()], None), Some(0));
        assert_eq!(pick_audio_stream(&[], None), None);
    }

    #[test]
    fn phone_tags_are_recognised() {
        let source = from_json(
            r#"{
                "streams": [{"codec_type": "video", "codec_name": "hevc", "pix_fmt": "yuv420p10le"}],
                "format": {"tags": {"com.apple.quicktime.make": "Apple"}}
            }"#,
        )
        .unwrap();
        assert!(source.from_camera);

        let source = from_json(
            r#"{
                "streams": [{"codec_type": "video", "codec_name": "h264", "pix_fmt": "yuv420p"}],
                "format": {"tags": {"com.android.version": "13"}}
            }"#,
        )
        .unwrap();
        assert!(source.from_camera);
    }

    #[test]
    fn full_chroma_reads_as_a_screen_recording() {
        assert!(looks_like_a_screen("yuv444p"));
        assert!(looks_like_a_screen("rgb24"));
        assert!(looks_like_a_screen("bgra"));
        assert!(!looks_like_a_screen("yuv420p"));
        assert!(!looks_like_a_screen("p010le"));
    }

    #[test]
    fn falls_back_to_pix_fmt_when_depth_tag_is_missing() {
        let source = from_json(
            r#"{"streams": [{"codec_type": "video", "pix_fmt": "yuv420p"}], "format": {}}"#,
        )
        .unwrap();
        assert_eq!(source.bit_depth, Some(8));
        assert!(!source.is_ten_bit());
        assert_eq!(source.creation_time, None);
        assert_eq!(source.duration, None);
        assert_eq!(source.audio_stream, None);
    }

    #[test]
    fn frame_rates_are_fractions() {
        assert_eq!(parse_frame_rate("30"), Some(30.0));
        assert_eq!(parse_frame_rate("60/1"), Some(60.0));
        assert!((parse_frame_rate("30000/1001").unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_frame_rate("0/0"), None);
        assert_eq!(parse_frame_rate("junk"), None);
    }

    #[test]
    fn geometry_and_bitrate_come_off_the_video_stream() {
        let source = from_json(
            r#"{
                "streams": [{"codec_type": "video", "codec_name": "h264", "pix_fmt": "yuv420p",
                             "width": 1920, "height": 1080, "avg_frame_rate": "30000/1001",
                             "bit_rate": "5000000"}],
                "format": {"duration": "60", "bit_rate": "5200000"}
            }"#,
        )
        .unwrap();
        assert_eq!(source.width, Some(1920));
        assert_eq!(source.height, Some(1080));
        assert_eq!(source.bit_rate, Some(5_000_000));
        let bpp = source.bits_per_pixel(0).unwrap();
        assert!((bpp - 0.0804).abs() < 0.001, "{bpp}");
    }

    #[test]
    fn the_container_bitrate_and_then_the_file_size_stand_in() {
        let from_format = from_json(
            r#"{
                "streams": [{"codec_type": "video", "pix_fmt": "yuv420p", "width": 100,
                             "height": 100, "avg_frame_rate": "10"}],
                "format": {"duration": "10", "bit_rate": "100000"}
            }"#,
        )
        .unwrap();
        assert_eq!(from_format.bit_rate, Some(100_000));
        assert_eq!(from_format.bits_per_pixel(0), Some(1.0));

        let from_size = from_json(
            r#"{
                "streams": [{"codec_type": "video", "pix_fmt": "yuv420p", "width": 100,
                             "height": 100, "avg_frame_rate": "10"}],
                "format": {"duration": "10"}
            }"#,
        )
        .unwrap();
        assert_eq!(from_size.bit_rate, None);
        assert_eq!(from_size.bits_per_pixel(125_000), Some(1.0));
    }

    #[test]
    fn a_file_without_geometry_has_no_weight() {
        let source = from_json(
            r#"{"streams": [{"codec_type": "video", "pix_fmt": "yuv420p"}], "format": {}}"#,
        )
        .unwrap();
        assert_eq!(source.bits_per_pixel(1_000_000), None);
    }

    fn lasting(seconds: f64) -> Source {
        Source {
            duration: Some(seconds),
            ..Source::default()
        }
    }

    #[test]
    fn an_output_of_the_right_length_passes() {
        assert_eq!(compare(&lasting(60.0), &lasting(60.0)), None);
        assert_eq!(compare(&lasting(60.0), &lasting(59.5)), None);
        assert_eq!(compare(&lasting(2.0), &lasting(2.4)), None);
    }

    #[test]
    fn a_truncated_output_is_caught() {
        let problem = compare(&lasting(60.0), &lasting(12.0)).unwrap();
        assert!(problem.contains("12.0s long"), "{problem}");
        assert!(compare(&lasting(60.0), &Source::default()).is_some());
    }

    #[test]
    fn an_output_that_lost_its_audio_is_caught() {
        let source = Source {
            audio_stream: Some(0),
            ..lasting(10.0)
        };
        assert!(compare(&source, &lasting(10.0)).is_some());
        assert_eq!(compare(&source, &source), None);
        assert_eq!(compare(&lasting(10.0), &source), None);
    }

    #[test]
    fn a_source_of_unknown_length_cannot_be_measured() {
        assert_eq!(compare(&Source::default(), &Source::default()), None);
    }

    #[test]
    fn audio_only_file_is_an_error() {
        let err = from_json(r#"{"streams": [{"codec_type": "audio"}]}"#).unwrap_err();
        assert!(err.to_string().contains("no video stream"));
    }
}
