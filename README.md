<div align="center">

# squish

**Batch video re-encoder on top of ffmpeg.**
One command, whole folder, originals untouched.

[![ci](https://github.com/MkSavin/squish/actions/workflows/ci.yml/badge.svg)](https://github.com/MkSavin/squish/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/MkSavin/squish?sort=semver)](https://github.com/MkSavin/squish/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![downloads](https://img.shields.io/github/downloads/MkSavin/squish/total)](https://github.com/MkSavin/squish/releases)

</div>

```console
$ squish
squish: 14 file(s), 2 job(s), gpu NVENC (NVIDIA GeForce RTX 4070 Ti)

  ... 7 done
  IMG_0307.MOV  phone  41.9 MiB -> 5.5 MiB (-86.9%)
  IMG_0308.MOV  phone  41.9 MiB -> 5.5 MiB (-86.9%)
  IMG_0309.MOV  phone  79.3 MiB -> 91.3 MiB (+15.1%)  (larger than the source, kept)
  clip-2019.mp4        skipped: already h264 at 0.031 bits/pixel
  broken.MOV           err: moov atom not found
  IMG_0310.MOV  phone  [====================    ]   83%   0.4x  hevc aac (apple_apac dropped)
  IMG_0311.MOV  web    [=====                   ]   21%   2.1x  h264 aac
  IMG_0312.MOV         pending
  ... 2 pending
```

`ffmpeg` can do all of this already. `squish` is the part you would otherwise
rewrite as a shell script every time: expanding the pattern, picking a pixel
format the encoder actually accepts, naming the outputs, skipping what is done,
cleaning up after a failure, and adding up what you saved.

## Features

- **Point it at a folder and walk away** — `squish` with no arguments takes every
  video in the current directory, picks a profile per file, and encodes two at a
  time.
- **Hardware by default** — NVENC on Windows and Linux, VideoToolbox on macOS,
  `--cpu` for libx264/libx265/libsvtav1.
- **Constant quality, not constant bitrate** — a static shot and a fast pan
  should not get the same bitrate. `-q 24` by default, `-b 8M` when you insist.
- **Profiles, chosen for you** — squish reads the file and picks `phone`,
  `screencast` or `web`; name one yourself with `-p archive` when you disagree.
- **One status list, docker-pull style** — one line per file: `pending`, a live
  bar with speed and codecs, or the size it ended up at. Too many files for the
  window, and finished and waiting split what the running ones leave.
- **Parallel encodes** — two at a time by default, `-j 4` for more.
- **Its own globbing** — `squish *.mov` works in cmd and PowerShell, which never
  expand patterns themselves.
- **Every result is checked** — ffmpeg can exit cleanly on a file that stops
  halfway or lost its sound. squish reads each output back and measures it
  against the source, so a short encode is a failure, not a green tick.
- **Nothing pointless gets re-encoded** — a file already light for its codec is
  left alone instead of being run through the encoder to come out worse.
- **Whole trees** — `-r` walks subdirectories, and `--out-dir` rebuilds the same
  folder structure on the other side.
- **Dates survive** — the output keeps its source's timestamp, so a re-encoded
  library sorts the way it did before.
- **Never touches the source** — output is `<name>.squish.mp4` next to it, a
  half-written file is deleted when ffmpeg fails, and one bad file does not stop
  the queue. `--trash-originals` is the one exception, and it only moves sources
  that produced a *verified* file in this very run, to the trash rather than nowhere.
- **Ctrl+C means stop** — squish kills the ffmpeg processes it started instead of
  leaving them running with nobody watching, and removes their half-written files.
- **Correct containers** — `hvc1` / `av01` tags, `+faststart`, so QuickTime and
  Safari open the result instead of showing a black rectangle.

## Installation

### Prebuilt binaries

Grab an archive for your platform from the
[latest release](https://github.com/MkSavin/squish/releases/latest) and put the
binary somewhere on your `PATH`.

| Platform | Archive |
| --- | --- |
| Windows x64 | `squish-<version>-x86_64-pc-windows-msvc.zip` |
| Linux x64 | `squish-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `squish-<version>-aarch64-apple-darwin.tar.gz` |

Every archive ships with a `.sha256` next to it.

### Package managers

Manifests for Scoop, Homebrew and winget live in [`packaging/`](packaging), ready
to submit — none of them is published yet, because there is no release to point
at. [`packaging/README.md`](packaging/README.md) has the steps.

### From source

```bash
cargo install --git https://github.com/MkSavin/squish
```

or

```bash
git clone https://github.com/MkSavin/squish
cd squish
cargo build --release
```

### Requirements

`ffmpeg` and `ffprobe` must be on your `PATH`; squish checks at startup and says
so plainly if they are not.

For the hardware path you also need a matching driver. ffmpeg is built against a
specific NVENC API version, and a driver older than that build fails with
`Driver does not support the required nvenc API version` — update the driver, or
use `--cpu`.

## Usage

```
squish [patterns...] [options]
```

Everything in this folder, profile chosen per file:

```bash
squish
```

Squeeze a folder of phone videos, keeping 10-bit:

```bash
squish -p phone *.MOV
```

Three at a time, into a separate folder:

```bash
squish -j 3 --out-dir compressed *.mp4
```

A whole tree, rebuilt under `compressed/`:

```bash
squish -r --out-dir compressed
```

One file, one name:

```bash
squish -o trailer-web.mp4 -p web raw-trailer.mov
```

See the commands without running anything:

```bash
squish --dry-run *.mkv
```

Feed the results to another tool:

```bash
squish --quiet *.mov | xargs -I{} rclone copy {} remote:backup
```

### Profiles

`-p auto` is the default and decides per file:

| The file looks like | Profile |
| --- | --- |
| 4:4:4 or RGB pixel format | `screencast` |
| phone capture tags, or 10-bit | `phone` |
| anything else | `web` |

| `-p` | Codec | Depth | Quality | Preset | For |
| --- | --- | --- | --- | --- | --- |
| `web` | h264 | 8-bit | 24 | p5 | plays anywhere, including old phones |
| `phone` | hevc | 10-bit | 26 | p5 | iPhone/Android footage |
| `screencast` | hevc | 8-bit | 22 | p6 | screen recordings, sharp text |
| `archive` | hevc | 10-bit | 18 | p7 | keep the detail, size second |
| `smallest` | av1 | 8-bit | 34 | p6 | smallest file, slowest encode |

A profile only fills in what you left out, so `-p archive -q 30` keeps hevc,
10-bit and p7 but encodes at CQ 30.

### Options

| Option | Meaning |
| --- | --- |
| `-q, --quality <N>` | constant quality 0-51, lower is better [default: 24] |
| `-b, --bitrate <RATE>` | VBR with this target instead, e.g. `8M`, `4500k` |
| `-c, --codec <CODEC>` | `h264`, `hevc`, `av1` |
| `-p, --profile <NAME>` | one of the profiles above |
| `-j, --jobs <N>` | encode N files at once [default: 2] |
| `-r, --recursive` | look in subdirectories too |
| `-o, --output <FILE>` | name the result; single input only |
| `--out-dir <DIR>` | write outputs here instead of next to the source |
| `--ten-bit` | keep 10-bit depth when the source has it |
| `--cpu` | libx264/libx265/libsvtav1 with CRF instead of the GPU |
| `--preset <P>` | `p1` (fastest) to `p7` (slowest) [default: p5] |
| `--trash-originals` | move every source re-encoded in this run to the trash |
| `--raw` | let ffmpeg write to the terminal itself; implies `-j 1` |
| `--quiet` | print nothing but the path of every file written |
| `--force` | overwrite existing outputs, and encode files that are already small |
| `--no-verify` | do not read the outputs back to check them |
| `--dry-run` | print the ffmpeg commands and stop |
| `--keep-metadata` | carry over container metadata and the capture timestamp |

## What squish decides for you

- **Pixel format is always explicit.** h264 through NVENC takes 8-bit 4:2:0 only,
  and handing it 10-bit input makes ffmpeg claim there is no NVENC device at all.
  squish reads the source format with ffprobe and sets `-pix_fmt` every time.
- **`--ten-bit` preserves, it does not invent.** An 8-bit source stays 8-bit, and
  says so.
- **Audio that cannot be decoded is dropped, not fatal.** iPhone clips carry a
  spatial `apple_apac` track next to the plain aac one; squish asks ffmpeg which
  decoders exist and maps the first track it can actually read.
- **Subtitles, chapters and data streams are dropped.** Audio becomes AAC 192k.
- **An output larger than its source is kept** and flagged in the summary. This
  is what `auto` exists to avoid: h264 on a 10-bit phone clip regularly grows it.
- **Every output is measured against its source.** ffmpeg returns 0 on a file
  that stopped early — a source truncated on disk still states its full length in
  the header, so the encode looks like a 45% saving and is really a 66% loss.
  squish compares the length and the presence of audio, and a mismatch is a
  failure with the output deleted. `--no-verify` turns the check off.
- **A file already light for its codec is skipped.** Below roughly 0.06 bits per
  pixel for h264, 0.04 for hevc, 0.03 for av1, and in a codec no weaker than the
  target, re-encoding only loses quality. `--force` does it anyway.
- **Outputs inherit the source's timestamp**, so a re-encoded library keeps its
  order. `--keep-metadata` is about the tags *inside* the file, and stays opt-in.
- **`--trash-originals` skips anything that failed, was skipped, or was
  interrupted.** A file only goes to the trash once a verified output exists for it.

## Releasing

Releases are driven by [release-plz](https://release-plz.dev): commit messages
decide the version, and merging a pull request cuts the release.

1. write commits as [conventional commits](https://www.conventionalcommits.org)
   — `fix:` bumps the patch, `feat:` the minor, a `!` or a `BREAKING CHANGE:`
   footer the major
2. on every push to `main`, release-plz keeps a *release PR* open with the next
   version written into `Cargo.toml` and the new `CHANGELOG.md` entries
3. merging that PR tags the commit, publishes the GitHub release, and the same
   workflow run attaches the binaries

Nothing is published to crates.io — `release-plz.toml` sets `git_only`, so the
git tag is the only source of version truth.

This needs **Settings -> Actions -> General -> Workflow permissions** with
read/write permissions and "Allow GitHub Actions to create and approve pull
requests" both enabled.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

`tests/e2e.rs` encodes real files with the ffmpeg on your `PATH` — a folder run,
a truncated source, recursion into `--out-dir`, a broken file, `--dry-run` and
`--quiet`. Without ffmpeg those tests print a line and skip themselves; CI
installs it on Windows, Linux and macOS, which is where the VideoToolbox path
gets exercised.

## License

MIT, see [LICENSE](LICENSE).
