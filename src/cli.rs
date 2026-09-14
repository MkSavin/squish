use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};

use crate::probe::Source;

pub const DEFAULT_QUALITY: u8 = 24;
pub const DEFAULT_PRESET: Preset = Preset(5);
pub const DEFAULT_JOBS: u16 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "squish",
    version,
    about = "Batch video re-encoder on top of ffmpeg (hardware constant quality by default)",
    after_help = "With no patterns, every video in the current directory is taken.\n\
                  Patterns are expanded by squish itself, because cmd and PowerShell do not do it."
)]
pub struct Cli {
    /// Files or glob patterns, e.g. *.mov [default: every video here]
    #[arg(value_name = "PATTERNS")]
    pub patterns: Vec<String>,

    /// Constant quality target, 0-51, lower is better [default: 24]
    #[arg(
        short = 'q',
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u8).range(0..=51),
    )]
    pub quality: Option<u8>,

    /// Target bitrate instead of constant quality, e.g. 8M or 4500k
    #[arg(short = 'b', long, value_name = "RATE", conflicts_with = "quality")]
    pub bitrate: Option<String>,

    /// Video codec [default: from the profile]
    #[arg(short = 'c', long, value_name = "CODEC")]
    pub codec: Option<Codec>,

    /// Settings for a kind of footage; `auto` reads it off the file
    #[arg(short = 'p', long, value_name = "NAME", default_value = "auto")]
    pub profile: Profile,

    /// Keep 10-bit depth when the source has it (needs hevc or av1)
    #[arg(long)]
    pub ten_bit: bool,

    /// Encode on the CPU (libx264/libx265/libsvtav1 + CRF) instead of the GPU
    #[arg(long)]
    pub cpu: bool,

    /// Encoder speed/quality preset, p1 (fastest) to p7 (slowest) [default: p5]
    #[arg(long, value_name = "P", value_parser = parse_preset)]
    pub preset: Option<Preset>,

    /// Look in subdirectories too
    #[arg(short = 'r', long)]
    pub recursive: bool,

    /// Files to encode at the same time
    #[arg(
        short = 'j',
        long,
        value_name = "N",
        default_value_t = DEFAULT_JOBS,
        value_parser = clap::value_parser!(u16).range(1..=64),
    )]
    pub jobs: u16,

    /// Write the result here; only valid for a single input file
    #[arg(short = 'o', long, value_name = "FILE", conflicts_with = "out_dir")]
    pub output: Option<PathBuf>,

    /// Write outputs into this directory instead of next to the source
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,

    /// Let ffmpeg write to the terminal itself instead of the status list; implies -j 1
    #[arg(long, conflicts_with = "quiet")]
    pub raw: bool,

    /// Print nothing but the path of every file written
    #[arg(long)]
    pub quiet: bool,

    /// Move every source that was re-encoded in this run to the trash, at the end
    #[arg(long)]
    pub trash_originals: bool,

    /// Overwrite existing outputs, and encode files that are already small
    #[arg(long)]
    pub force: bool,

    /// Do not read the outputs back to check their length and audio
    #[arg(long)]
    pub no_verify: bool,

    /// Print the ffmpeg commands without running them
    #[arg(long)]
    pub dry_run: bool,

    /// Carry over container metadata and the capture timestamp
    #[arg(long)]
    pub keep_metadata: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

/// Where the encoding happens. Picked from the platform unless `--cpu` is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Nvenc,
    VideoToolbox,
    Cpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Profile {
    /// pick per file from what ffprobe reports
    Auto,
    /// h264 8-bit, plays anywhere
    Web,
    /// phone footage: hevc, keeps the original look
    Phone,
    /// screen recordings: sharp text, slow preset
    Screencast,
    /// keep as much detail as is reasonable
    Archive,
    /// av1, smallest files, slowest to encode
    Smallest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    Quality(u8),
    Bitrate(u64),
}

/// What the command line asked for, before a profile fills in the gaps.
#[derive(Debug, Clone)]
struct Requested {
    codec: Option<Codec>,
    rate: Option<RateControl>,
    preset: Option<Preset>,
    ten_bit: bool,
}

/// Settings that do not depend on any single file.
#[derive(Debug, Clone)]
pub struct Config {
    requested: Requested,
    profile: Profile,
    pub backend: Backend,
    pub jobs: usize,
    pub output: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    /// Directory the inputs share, so `--out-dir` can mirror their tree.
    pub out_base: Option<PathBuf>,
    pub recursive: bool,
    pub raw: bool,
    pub quiet: bool,
    pub trash_originals: bool,
    pub force: bool,
    pub verify: bool,
    pub dry_run: bool,
    pub keep_metadata: bool,
}

/// How one particular file gets encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub codec: Codec,
    pub backend: Backend,
    pub rate: RateControl,
    pub preset: Preset,
    pub ten_bit: bool,
    pub keep_metadata: bool,
    pub profile: Profile,
}

struct ProfileDefaults {
    codec: Codec,
    quality: u8,
    preset: Preset,
    ten_bit: bool,
}

impl Profile {
    fn defaults(self) -> ProfileDefaults {
        match self {
            Profile::Auto | Profile::Web => ProfileDefaults {
                codec: Codec::H264,
                quality: DEFAULT_QUALITY,
                preset: DEFAULT_PRESET,
                ten_bit: false,
            },
            Profile::Phone => ProfileDefaults {
                codec: Codec::Hevc,
                quality: 26,
                preset: Preset(5),
                ten_bit: true,
            },
            Profile::Screencast => ProfileDefaults {
                codec: Codec::Hevc,
                quality: 22,
                preset: Preset(6),
                ten_bit: false,
            },
            Profile::Archive => ProfileDefaults {
                codec: Codec::Hevc,
                quality: 18,
                preset: Preset(7),
                ten_bit: true,
            },
            Profile::Smallest => ProfileDefaults {
                codec: Codec::Av1,
                quality: 34,
                preset: Preset(6),
                ten_bit: false,
            },
        }
    }

    /// h264 on 10-bit or phone footage tends to grow the file, so those go to hevc.
    pub fn for_source(source: &Source) -> Profile {
        if source.screen_like {
            Profile::Screencast
        } else if source.from_camera || source.is_ten_bit() {
            Profile::Phone
        } else {
            Profile::Web
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Auto => "auto",
            Profile::Web => "web",
            Profile::Phone => "phone",
            Profile::Screencast => "screencast",
            Profile::Archive => "archive",
            Profile::Smallest => "smallest",
        }
    }
}

impl Cli {
    pub fn resolve(self) -> Result<(Vec<String>, Config)> {
        let rate = match &self.bitrate {
            Some(raw) => Some(RateControl::Bitrate(
                parse_bitrate(raw).with_context(|| format!("bad --bitrate value {raw:?}"))?,
            )),
            None => self.quality.map(RateControl::Quality),
        };

        let backend = if self.cpu {
            Backend::Cpu
        } else if cfg!(target_os = "macos") {
            Backend::VideoToolbox
        } else {
            Backend::Nvenc
        };
        if self.ten_bit && self.codec == Some(Codec::H264) {
            bail!("--ten-bit needs hevc or av1: h264 is 8-bit 4:2:0 only");
        }

        let config = Config {
            requested: Requested {
                codec: self.codec,
                rate,
                preset: self.preset,
                ten_bit: self.ten_bit,
            },
            profile: self.profile,
            backend,
            // Raw ffmpeg output from several encodes at once is unreadable.
            jobs: if self.raw { 1 } else { self.jobs as usize },
            output: self.output,
            out_dir: self.out_dir,
            out_base: None,
            recursive: self.recursive,
            raw: self.raw,
            quiet: self.quiet,
            trash_originals: self.trash_originals,
            force: self.force,
            verify: !self.no_verify,
            dry_run: self.dry_run,
            keep_metadata: self.keep_metadata,
        };
        config.check_backend()?;
        Ok((self.patterns, config))
    }
}

impl Config {
    /// Merges the command line with the profile for one file.
    pub fn plan_for(&self, source: &Source) -> Plan {
        let profile = match self.profile {
            Profile::Auto => Profile::for_source(source),
            fixed => fixed,
        };
        let defaults = profile.defaults();

        let codec = self.requested.codec.unwrap_or(defaults.codec);
        let ten_bit = (self.requested.ten_bit || defaults.ten_bit) && codec.supports_ten_bit();

        Plan {
            codec,
            backend: self.backend,
            rate: self
                .requested
                .rate
                .unwrap_or(RateControl::Quality(defaults.quality)),
            preset: self.requested.preset.unwrap_or(defaults.preset),
            ten_bit,
            keep_metadata: self.keep_metadata,
            profile,
        }
    }

    /// The one combination that can never work, caught before any file is touched.
    fn check_backend(&self) -> Result<()> {
        let av1_requested = self.requested.codec == Some(Codec::Av1)
            || matches!(self.profile, Profile::Smallest) && self.requested.codec.is_none();
        if self.backend == Backend::VideoToolbox && av1_requested {
            bail!("VideoToolbox has no av1 encoder; add --cpu to use libsvtav1");
        }
        Ok(())
    }
}

impl Codec {
    pub fn encoder(self, backend: Backend) -> &'static str {
        match (self, backend) {
            (Codec::H264, Backend::Nvenc) => "h264_nvenc",
            (Codec::Hevc, Backend::Nvenc) => "hevc_nvenc",
            (Codec::Av1, Backend::Nvenc) => "av1_nvenc",
            (Codec::H264, Backend::VideoToolbox) => "h264_videotoolbox",
            (Codec::Hevc, Backend::VideoToolbox) => "hevc_videotoolbox",
            (Codec::Av1, Backend::VideoToolbox) => "libsvtav1",
            (Codec::H264, Backend::Cpu) => "libx264",
            (Codec::Hevc, Backend::Cpu) => "libx265",
            (Codec::Av1, Backend::Cpu) => "libsvtav1",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
        }
    }

    /// mp4 brand tag QuickTime and Safari need to recognise the track.
    pub fn mp4_tag(self) -> Option<&'static str> {
        match self {
            Codec::H264 => None,
            Codec::Hevc => Some("hvc1"),
            Codec::Av1 => Some("av01"),
        }
    }

    pub fn supports_ten_bit(self) -> bool {
        !matches!(self, Codec::H264)
    }

    /// What ffprobe calls the codec, for the ones squish can also produce.
    fn from_stream(name: &str) -> Option<Codec> {
        match name {
            "h264" | "avc1" => Some(Codec::H264),
            "hevc" | "h265" => Some(Codec::Hevc),
            "av1" => Some(Codec::Av1),
            _ => None,
        }
    }

    /// How much a codec squeezes out of the same bits.
    fn rank(self) -> u8 {
        match self {
            Codec::H264 => 0,
            Codec::Hevc => 1,
            Codec::Av1 => 2,
        }
    }

    /// Bits per pixel below which this codec is already at its useful limit.
    fn bpp_floor(self) -> f64 {
        match self {
            Codec::H264 => 0.06,
            Codec::Hevc => 0.04,
            Codec::Av1 => 0.03,
        }
    }
}

impl Plan {
    /// A file already light, in a codec no worse than the target, only loses
    /// quality on the way through.
    pub fn pointless_for(&self, source: &Source, bytes: u64) -> Option<String> {
        let current = Codec::from_stream(source.video_codec.as_deref()?)?;
        if current.rank() < self.codec.rank() {
            return None;
        }
        let bpp = source.bits_per_pixel(bytes)?;
        (bpp < self.codec.bpp_floor()).then(|| {
            let weight = match bpp < 0.001 {
                true => "under 0.001".to_owned(),
                false => format!("{bpp:.3}"),
            };
            format!(
                "already {} at {weight} bits/pixel",
                source.video_codec.as_deref().unwrap_or("encoded")
            )
        })
    }
}

impl Preset {
    /// x264/x265 take named presets; p1 is the fastest end of both scales.
    pub fn x26x(self) -> &'static str {
        match self.0 {
            1 => "ultrafast",
            2 => "superfast",
            3 => "veryfast",
            4 => "faster",
            5 => "medium",
            6 => "slow",
            _ => "slower",
        }
    }

    /// SVT-AV1 counts the other way round: 0 is slowest, 13 is fastest.
    pub fn svtav1(self) -> &'static str {
        match self.0 {
            1 => "10",
            2 => "9",
            3 => "8",
            4 => "7",
            5 => "6",
            6 => "4",
            _ => "2",
        }
    }

    pub fn nvenc(self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "p{}", self.0)
    }
}

fn parse_preset(raw: &str) -> Result<Preset, String> {
    let digits = raw
        .strip_prefix('p')
        .or_else(|| raw.strip_prefix('P'))
        .unwrap_or(raw);
    match digits.parse::<u8>() {
        Ok(n @ 1..=7) => Ok(Preset(n)),
        _ => Err(format!("expected p1 to p7, got {raw:?}")),
    }
}

/// Accepts plain bits per second plus k/M/G suffixes, e.g. `8M`, `4500k`, `4.5M`.
pub fn parse_bitrate(raw: &str) -> Result<u64> {
    let trimmed = raw.trim();
    let last = trimmed.chars().next_back().context("empty bitrate")?;
    let (number, multiplier) = match last {
        'k' | 'K' => (&trimmed[..trimmed.len() - 1], 1_000f64),
        'm' | 'M' => (&trimmed[..trimmed.len() - 1], 1_000_000f64),
        'g' | 'G' => (&trimmed[..trimmed.len() - 1], 1_000_000_000f64),
        '0'..='9' => (trimmed, 1f64),
        _ => bail!("expected digits with an optional k, M or G suffix"),
    };

    if !number.chars().all(|c| c.is_ascii_digit() || c == '.') {
        bail!("expected digits with an optional k, M or G suffix");
    }
    let value: f64 = number
        .parse()
        .map_err(|_| anyhow::anyhow!("{number:?} is not a number"))?;
    if !value.is_finite() || value <= 0.0 {
        bail!("bitrate must be positive");
    }
    let bits = value * multiplier;
    if bits >= u64::MAX as f64 {
        bail!("bitrate is absurdly large");
    }
    Ok(bits.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(args: &[&str]) -> Result<Config> {
        let mut argv = vec!["squish"];
        argv.extend_from_slice(args);
        argv.push("a.mov");
        Ok(Cli::try_parse_from(argv)?.resolve()?.1)
    }

    fn plan(args: &[&str], source: &Source) -> Plan {
        config(args).unwrap().plan_for(source)
    }

    fn plain() -> Source {
        Source {
            pix_fmt: Some("yuv420p".into()),
            bit_depth: Some(8),
            ..Source::default()
        }
    }

    fn phone_clip() -> Source {
        Source {
            pix_fmt: Some("yuv420p10le".into()),
            bit_depth: Some(10),
            from_camera: true,
            ..Source::default()
        }
    }

    fn screen_capture() -> Source {
        Source {
            pix_fmt: Some("yuv444p".into()),
            bit_depth: Some(8),
            screen_like: true,
            ..Source::default()
        }
    }

    #[test]
    fn bitrate_suffixes() {
        assert_eq!(parse_bitrate("8M").unwrap(), 8_000_000);
        assert_eq!(parse_bitrate("8m").unwrap(), 8_000_000);
        assert_eq!(parse_bitrate("4500k").unwrap(), 4_500_000);
        assert_eq!(parse_bitrate("4500K").unwrap(), 4_500_000);
        assert_eq!(parse_bitrate("6000000").unwrap(), 6_000_000);
        assert_eq!(parse_bitrate("1G").unwrap(), 1_000_000_000);
        assert_eq!(parse_bitrate("  8M  ").unwrap(), 8_000_000);
    }

    #[test]
    fn bitrate_fractions() {
        assert_eq!(parse_bitrate("4.5M").unwrap(), 4_500_000);
        assert_eq!(parse_bitrate("0.5M").unwrap(), 500_000);
    }

    #[test]
    fn bitrate_rejects_junk() {
        for bad in [
            "", "   ", "abc", "8MB", "8 M", "-5M", "0", "0k", "M", "8.5.5M", "+8M", "8Mb",
        ] {
            assert!(parse_bitrate(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn preset_parsing() {
        assert_eq!(parse_preset("p5"), Ok(Preset(5)));
        assert_eq!(parse_preset("P1"), Ok(Preset(1)));
        assert_eq!(parse_preset("7"), Ok(Preset(7)));
        assert!(parse_preset("p0").is_err());
        assert!(parse_preset("p8").is_err());
        assert!(parse_preset("slow").is_err());
    }

    #[test]
    fn two_jobs_by_default_and_patterns_are_optional() {
        let cli = Cli::try_parse_from(["squish"]).unwrap();
        let (patterns, cfg) = cli.resolve().unwrap();
        assert!(patterns.is_empty());
        assert_eq!(cfg.jobs, DEFAULT_JOBS as usize);
    }

    #[test]
    fn a_plain_file_gets_plain_h264() {
        let plan = plan(&[], &plain());
        assert_eq!(plan.codec, Codec::H264);
        assert_eq!(plan.rate, RateControl::Quality(DEFAULT_QUALITY));
        assert_eq!(plan.preset, DEFAULT_PRESET);
        assert!(!plan.ten_bit);
        assert_eq!(plan.profile, Profile::Web);
    }

    #[test]
    fn auto_sends_phone_footage_to_hevc() {
        let plan = plan(&[], &phone_clip());
        assert_eq!(plan.codec, Codec::Hevc);
        assert_eq!(plan.rate, RateControl::Quality(26));
        assert!(plan.ten_bit);
        assert_eq!(plan.profile, Profile::Phone);
    }

    #[test]
    fn auto_sends_screen_recordings_to_the_screencast_profile() {
        let plan = plan(&[], &screen_capture());
        assert_eq!(plan.codec, Codec::Hevc);
        assert_eq!(plan.rate, RateControl::Quality(22));
        assert_eq!(plan.preset, Preset(6));
        assert!(!plan.ten_bit);
        assert_eq!(plan.profile, Profile::Screencast);
    }

    #[test]
    fn a_ten_bit_source_without_camera_tags_still_avoids_h264() {
        let source = Source {
            pix_fmt: Some("yuv420p10le".into()),
            bit_depth: Some(10),
            ..Source::default()
        };
        assert_eq!(Profile::for_source(&source), Profile::Phone);
    }

    #[test]
    fn a_named_profile_stops_the_guessing() {
        let plan = plan(&["-p", "archive"], &plain());
        assert_eq!(plan.codec, Codec::Hevc);
        assert_eq!(plan.rate, RateControl::Quality(18));
        assert_eq!(plan.preset, Preset(7));
        assert_eq!(plan.profile, Profile::Archive);
    }

    #[test]
    fn explicit_flags_win_over_any_profile() {
        let plan = plan(&["-p", "archive", "-q", "30", "--preset", "p1"], &plain());
        assert_eq!(plan.rate, RateControl::Quality(30));
        assert_eq!(plan.preset, Preset(1));
        assert_eq!(plan.codec, Codec::Hevc);
    }

    #[test]
    fn an_explicit_eight_bit_codec_turns_off_the_profile_ten_bit() {
        let plan = plan(&["-c", "h264"], &phone_clip());
        assert_eq!(plan.codec, Codec::H264);
        assert!(!plan.ten_bit);
    }

    #[test]
    fn ten_bit_with_h264_is_refused_outright() {
        assert!(config(&["--ten-bit", "-c", "h264"]).is_err());
        assert!(config(&["--ten-bit", "-c", "hevc"]).is_ok());
    }

    #[test]
    fn bitrate_flag_switches_rate_control() {
        let plan = plan(&["-b", "8M"], &phone_clip());
        assert_eq!(plan.rate, RateControl::Bitrate(8_000_000));
    }

    #[test]
    fn quality_and_bitrate_conflict() {
        assert!(config(&["-q", "20", "-b", "8M"]).is_err());
    }

    #[test]
    fn output_and_out_dir_conflict() {
        assert!(config(&["-o", "x.mp4", "--out-dir", "d"]).is_err());
    }

    #[test]
    fn raw_and_quiet_conflict() {
        assert!(config(&["--raw", "--quiet"]).is_err());
    }

    #[test]
    fn cpu_backend_is_opt_in() {
        assert_eq!(config(&["--cpu"]).unwrap().backend, Backend::Cpu);
        let native = config(&[]).unwrap().backend;
        if cfg!(target_os = "macos") {
            assert_eq!(native, Backend::VideoToolbox);
        } else {
            assert_eq!(native, Backend::Nvenc);
        }
    }

    #[test]
    fn videotoolbox_has_no_av1_encoder() {
        let refused = config(&["-c", "av1"]).is_err();
        assert_eq!(refused, cfg!(target_os = "macos"));
        assert!(config(&["-c", "av1", "--cpu"]).is_ok());
    }

    #[test]
    fn raw_output_falls_back_to_one_job() {
        assert_eq!(config(&["--raw", "-j", "4"]).unwrap().jobs, 1);
    }

    #[test]
    fn trashing_originals_is_off_unless_asked_for() {
        assert!(!config(&[]).unwrap().trash_originals);
        assert!(config(&["--trash-originals"]).unwrap().trash_originals);
    }

    fn already_small(codec: &str, bit_rate: u64) -> Source {
        Source {
            video_codec: Some(codec.into()),
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            bit_rate: Some(bit_rate),
            ..plain()
        }
    }

    #[test]
    fn a_lean_file_in_the_same_codec_is_not_worth_redoing() {
        // 1080p30 at 2 Mbit/s is 0.032 bits per pixel.
        let plan = plan(&["-c", "hevc"], &plain());
        let reason = plan
            .pointless_for(&already_small("hevc", 2_000_000), 0)
            .unwrap();
        assert!(reason.contains("already hevc"), "{reason}");
        assert!(reason.contains("0.032"), "{reason}");
    }

    #[test]
    fn a_nearly_empty_file_does_not_read_as_zero() {
        let plan = plan(&["-c", "h264"], &plain());
        let reason = plan
            .pointless_for(&already_small("h264", 20_000), 0)
            .unwrap();
        assert!(reason.contains("under 0.001"), "{reason}");
    }

    #[test]
    fn a_fat_file_is_always_worth_redoing() {
        let plan = plan(&["-c", "hevc"], &plain());
        assert_eq!(
            plan.pointless_for(&already_small("hevc", 20_000_000), 0),
            None
        );
    }

    #[test]
    fn a_lean_file_in_a_weaker_codec_still_gets_encoded() {
        let to_hevc = plan(&["-c", "hevc"], &plain());
        assert_eq!(
            to_hevc.pointless_for(&already_small("h264", 2_000_000), 0),
            None
        );

        let to_h264 = plan(&["-c", "h264"], &plain());
        assert!(to_h264
            .pointless_for(&already_small("hevc", 2_000_000), 0)
            .is_some());
    }

    #[test]
    fn a_codec_squish_cannot_produce_is_never_left_alone() {
        let plan = plan(&["-c", "hevc"], &plain());
        assert_eq!(plan.pointless_for(&already_small("prores", 1), 0), None);
        assert_eq!(plan.pointless_for(&plain(), 0), None);
    }

    #[test]
    fn verification_is_on_unless_waived() {
        assert!(config(&[]).unwrap().verify);
        assert!(!config(&["--no-verify"]).unwrap().verify);
    }

    #[test]
    fn recursion_is_opt_in() {
        assert!(!config(&[]).unwrap().recursive);
        assert!(config(&["-r"]).unwrap().recursive);
        assert!(config(&["--recursive"]).unwrap().recursive);
    }

    #[test]
    fn jobs_must_be_positive() {
        assert_eq!(config(&["-j", "4"]).unwrap().jobs, 4);
        assert!(config(&["-j", "0"]).is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory as _;
        Cli::command().debug_assert();
    }
}
