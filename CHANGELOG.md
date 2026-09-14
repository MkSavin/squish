# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `--trash-originals` moves the sources that produced a file in this run to the
  trash once everything is done. Files that failed, were skipped or interrupted
  keep their original, and nothing is ever deleted outright.
- The header line names what the work runs on, e.g.
  `gpu NVENC (NVIDIA GeForce RTX 4070 Ti)` or `cpu (AMD Ryzen 9 5900X)`.
- A status list in the style of `docker pull`: one line per input, carrying its
  profile, and `pending`, a live bar with percentage, speed and target codecs
  (with anything dropped named next to them), or its final size change. When the
  list outgrows the window, finished and waiting files split the space the
  running ones leave, half each, and either side takes what the other does not
  use. `--raw` gives ffmpeg the terminal back (and drops to one job), `--quiet`
  prints only the paths written.
- `squish` with no patterns takes every video in the current directory.
- `-p auto`, now the default, reads the profile off each file: 4:4:4 or RGB is a
  screen recording, phone capture tags or 10-bit go to hevc, the rest to h264.
  This stops h264 from inflating 10-bit phone footage.
- `-j, --jobs <N>` encodes several files at once, with one aggregated progress
  block parsed from ffmpeg's `-progress` output instead of interleaved logs.
  now two by default, which measured about 30% faster than one on an RTX 4070 Ti.
- `-p, --profile <NAME>`: `web`, `phone`, `screencast`, `archive`, `smallest`.
  A profile only fills in what the command line left out.
- `-o, --output <FILE>` names the result of a single input file.
- VideoToolbox is used on macOS the way NVENC is used elsewhere, and macOS
  arm64 binaries are built alongside the Windows and Linux ones.

### Fixed

- Ctrl+C now stops the ffmpeg processes squish started. The console signal alone
  did not reliably reach them, which left an encode running with nothing watching
  it; squish now kills its children itself, drops their half-written outputs and
  leaves the rest of the queue untouched. A second Ctrl+C exits immediately.
- The status list no longer flickers. It used to erase the whole block and write
  it again, which puts an empty frame on screen between the two; now the cursor
  goes back to the top of the block and only the lines that actually changed are
  overwritten, each cut to the terminal width so nothing wraps, in a single write.
- Files with an audio track ffmpeg cannot decode — iPhone clips carry a spatial
  `apple_apac` track next to the plain aac one — no longer fail with
  `Decoding requested, but no decoder found`. The decoder list is queried once
  and the first readable audio track is mapped by index; the rest are dropped
  with a note, and a file with no readable audio is encoded without sound.

## [0.1.0] - 2026-09-14

### Added

- Batch re-encoding of every file matching the given patterns, one ffmpeg run at a time.
- Own glob expansion, because cmd and PowerShell pass patterns through untouched;
  a pattern that matches nothing but exists on disk is taken literally.
- Constant quality by default (`-rc vbr -cq N -b:v 0`), `--bitrate` switches to VBR.
- NVENC h264/hevc/av1, `--cpu` falls back to libx264/libx265/libsvtav1 with CRF.
- Source pixel format detected through ffprobe, `-pix_fmt` always set explicitly;
  `--ten-bit` keeps 10-bit depth as `p010le` + `-profile:v main10`.
- `hvc1` / `av01` tags so QuickTime and Safari open the results.
- Outputs written as `<name>.squish.mp4`; sources are never touched, existing
  outputs are skipped without `--force`, and outputs of earlier runs are ignored.
- Truncated outputs are removed when ffmpeg exits non-zero; one failure does not
  stop the queue, and the exit code is non-zero if any file failed.
- Per-file and total size report, with files that grew flagged.

[Unreleased]: https://github.com/MkSavin/squish/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/MkSavin/squish/releases/tag/v0.1.0
