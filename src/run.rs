use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::cli::Config;
use crate::progress::{self, Display, Row, Status, Tick};
use crate::report::{change, human_size, Outcome, Report};
use crate::{expand, ffmpeg, probe, shutdown};

const REDRAW_EVERY: Duration = Duration::from_millis(100);

enum Event {
    Note {
        index: usize,
        text: String,
    },
    Started {
        index: usize,
        profile: String,
        codecs: String,
        duration: Option<f64>,
    },
    Progress {
        index: usize,
        tick: Tick,
    },
    Finished {
        index: usize,
        outcome: Outcome,
        output: Option<PathBuf>,
    },
}

enum Step {
    Skipped(String),
    Encoded { input: u64, output: u64 },
}

pub fn encode_all(inputs: &[PathBuf], config: &Config) -> Report {
    let live = !config.raw && !config.quiet && !config.dry_run && Display::is_live();
    let mut rows: Vec<Row> = inputs.iter().map(|p| Row::new(file_name(p))).collect();
    let mut outputs: Vec<Option<PathBuf>> = vec![None; inputs.len()];
    let mut results: Vec<Option<Outcome>> = vec![None; inputs.len()];

    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();

    std::thread::scope(|scope| {
        for _ in 0..config.jobs.min(inputs.len().max(1)) {
            let tx = tx.clone();
            let next = &next;
            scope.spawn(move || loop {
                if shutdown::requested() {
                    return;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(input) = inputs.get(index) else {
                    return;
                };
                work(index, input, config, &tx);
            });
        }
        drop(tx);

        let mut display = Display::default();
        let mut last_draw = Instant::now() - REDRAW_EVERY;

        while let Ok(event) = rx.recv() {
            let mut structural = true;
            match event {
                Event::Note { index, text } => {
                    if live {
                        rows[index].notes.push(text);
                    } else if !config.quiet {
                        println!("  {}: {text}", rows[index].name);
                    }
                }
                Event::Started {
                    index,
                    profile,
                    codecs,
                    duration,
                } => {
                    rows[index].profile = Some(profile);
                    rows[index].status = Status::Active {
                        tick: Tick::default(),
                        duration,
                        codecs,
                    };
                }
                Event::Progress { index, tick } => {
                    if let Status::Active { tick: slot, .. } = &mut rows[index].status {
                        *slot = tick;
                    }
                    structural = false;
                }
                Event::Finished {
                    index,
                    outcome,
                    output,
                } => {
                    rows[index].status = status_of(&outcome);
                    if !live && !config.quiet {
                        let row = &rows[index];
                        match &row.profile {
                            Some(profile) => {
                                println!("  {} [{profile}]: {}", row.name, row.status.text())
                            }
                            None => println!("  {}: {}", row.name, row.status.text()),
                        }
                    }
                    outputs[index] = output;
                    results[index] = Some(outcome);
                }
            }

            if live && (structural || last_draw.elapsed() >= REDRAW_EVERY) {
                display.draw(&progress::render(&rows, progress::viewport()));
                last_draw = Instant::now();
            }
        }

        if live {
            display.clear();
            for line in progress::render(&rows, usize::MAX) {
                println!("{line}");
            }
        }
    });

    let mut report = Report::default();
    for ((input, outcome), output) in inputs.iter().zip(results).zip(outputs) {
        if let Some(outcome) = outcome {
            report.push(input, outcome, output);
        }
    }
    report
}

fn status_of(outcome: &Outcome) -> Status {
    match outcome {
        Outcome::Encoded { input, output } => {
            let grown = if output > input {
                "  (larger than the source, kept)"
            } else {
                ""
            };
            Status::Done(format!(
                "{} -> {} ({}){grown}",
                human_size(*input),
                human_size(*output),
                change(*input, *output)
            ))
        }
        Outcome::Skipped { reason } => Status::Skipped(format!("skipped: {reason}")),
        Outcome::Failed { error } => Status::Failed(error.clone()),
    }
}

fn work(index: usize, input: &Path, config: &Config, tx: &Sender<Event>) {
    let mut written = None;
    let outcome = match process(index, input, config, tx, &mut written) {
        Ok(Step::Skipped(reason)) => Outcome::Skipped { reason },
        Ok(Step::Encoded { input, output }) => Outcome::Encoded { input, output },
        Err(error) => Outcome::Failed {
            error: format!("{error:#}"),
        },
    };
    let _ = tx.send(Event::Finished {
        index,
        outcome,
        output: written,
    });
}

fn process(
    index: usize,
    input: &Path,
    config: &Config,
    tx: &Sender<Event>,
    written: &mut Option<PathBuf>,
) -> Result<Step> {
    let note = |text: String| {
        let _ = tx.send(Event::Note { index, text });
    };

    if expand::is_squish_output(input) {
        return Ok(Step::Skipped("already a squish output".into()));
    }

    let output = match &config.output {
        Some(path) => path.clone(),
        None => expand::output_path(input, config.out_dir.as_deref(), config.out_base.as_deref())?,
    };
    if same_file(input, &output) {
        bail!("output {} would overwrite the source", output.display());
    }
    if output.exists() && !config.force {
        return Ok(Step::Skipped(format!(
            "{} exists, use --force",
            output.display()
        )));
    }

    let source =
        probe::probe(input).with_context(|| format!("cannot probe {}", input.display()))?;
    let plan = config.plan_for(&source);
    ffmpeg::check_encoder(&plan)?;

    let before = file_size(input)?;
    if !config.force {
        if let Some(reason) = plan.pointless_for(&source, before) {
            return Ok(Step::Skipped(reason));
        }
    }
    if plan.ten_bit && !source.is_ten_bit() {
        note(format!(
            "source is {} bit, encoding 8 bit",
            source.bit_depth.unwrap_or(8)
        ));
    }
    let args = ffmpeg::build_args(input, &output, &plan, &source);
    if config.dry_run {
        println!("  {}", ffmpeg::display_command(&args));
        return Ok(Step::Skipped("dry run".into()));
    }
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    let _ = tx.send(Event::Started {
        index,
        profile: plan.profile.name().to_owned(),
        codecs: describe_streams(&plan, &source),
        duration: source.duration,
    });

    match encode(index, &args, config, tx, &note) {
        Err(error) => {
            discard_partial(&output, &note);
            return Err(error).with_context(|| format!("encoding {}", input.display()));
        }
        Ok(ffmpeg::Completion::Interrupted) => {
            discard_partial(&output, &note);
            return Ok(Step::Skipped("interrupted".into()));
        }
        Ok(ffmpeg::Completion::Finished) => {}
    }

    if config.verify {
        if let Err(error) = probe::verify(&output, &source) {
            discard_partial(&output, &note);
            return Err(error).with_context(|| format!("checking {}", output.display()));
        }
    }
    copy_timestamp(input, &output, &note);

    let after = file_size(&output)?;
    *written = Some(output);
    Ok(Step::Encoded {
        input: before,
        output: after,
    })
}

const BUSY_RETRIES: u32 = 3;
const BUSY_WAIT: Duration = Duration::from_secs(3);

/// A GPU with every encoder session taken frees one as soon as a neighbour
/// finishes, so the file waits instead of failing.
fn encode(
    index: usize,
    args: &[std::ffi::OsString],
    config: &Config,
    tx: &Sender<Event>,
    note: &dyn Fn(String),
) -> Result<ffmpeg::Completion> {
    let with_progress = ffmpeg::with_progress(args);
    for attempt in 1..=BUSY_RETRIES {
        let result = if config.raw {
            ffmpeg::run(args)
        } else {
            ffmpeg::run_watched(&with_progress, |tick| {
                let _ = tx.send(Event::Progress { index, tick });
            })
        };
        match result {
            Err(error) if attempt < BUSY_RETRIES && ffmpeg::is_busy(&error) => {
                note(format!("{}, waiting", ffmpeg::BUSY));
                if !shutdown::sleep(BUSY_WAIT) {
                    return Ok(ffmpeg::Completion::Interrupted);
                }
            }
            other => return other,
        }
    }
    unreachable!("the last attempt returns")
}

/// Keeps the result sorted next to its neighbours: a re-encode is the same
/// recording, not a new one.
fn copy_timestamp(input: &Path, output: &Path, note: &dyn Fn(String)) {
    let stamped = fs::metadata(input)
        .and_then(|meta| meta.modified())
        .and_then(|time| {
            fs::File::options()
                .write(true)
                .open(output)
                .and_then(|file| file.set_modified(time))
        });
    if let Err(error) = stamped {
        note(format!("cannot copy the timestamp: {error}"));
    }
}

/// What the output will hold, plus anything of the input that had to go.
fn describe_streams(plan: &crate::cli::Plan, source: &probe::Source) -> String {
    let mut text = plan.codec.name().to_owned();
    if source.audio_stream.is_some() {
        text.push_str(" aac");
    }
    if !source.dropped_audio.is_empty() {
        text.push_str(&format!(" ({} dropped)", source.dropped_audio.join(", ")));
    }
    text
}

/// A truncated file is worse than no file, so it goes when ffmpeg fails.
fn discard_partial(output: &Path, note: &dyn Fn(String)) {
    if output.exists() {
        if let Err(error) = fs::remove_file(output) {
            note(format!("cannot remove {}: {error}", output.display()));
        }
    }
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)
        .with_context(|| format!("cannot stat {}", path.display()))?
        .len())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
