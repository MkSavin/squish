mod cli;
mod device;
mod expand;
mod ffmpeg;
mod probe;
mod progress;
mod report;
mod run;
mod shutdown;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Parser;

use cli::{Cli, Config};
use report::Report;

fn main() -> ExitCode {
    match execute() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("squish: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<bool> {
    let (patterns, config) = Cli::parse().resolve()?;
    ffmpeg::preflight()?;
    shutdown::install();

    let inputs = collect(&patterns)?;
    if config.output.is_some() && inputs.len() > 1 {
        bail!(
            "--output names one file, but {} matched; use --out-dir instead",
            inputs.len()
        );
    }
    if let Some(dir) = &config.out_dir {
        fs::create_dir_all(dir)
            .with_context(|| format!("cannot create output directory {}", dir.display()))?;
    }

    if !config.quiet {
        println!(
            "squish: {} file(s), {} job(s), {}
",
            inputs.len(),
            config.jobs.min(inputs.len()),
            device::describe(config.backend)
        );
    }

    let report = run::encode_all(&inputs, &config);
    if config.quiet {
        for path in report.outputs() {
            println!("{}", path.display());
        }
    } else {
        report.print();
    }
    if config.trash_originals {
        trash_originals(&report, &config);
    }
    Ok(!report.has_failures() && !shutdown::requested())
}

/// No patterns at all means "every video in this directory".
fn collect(patterns: &[String]) -> Result<Vec<PathBuf>> {
    if patterns.is_empty() {
        let here = std::env::current_dir().context("cannot read the current directory")?;
        let files = expand::videos_in(&here)?;
        if files.is_empty() {
            bail!("no video files in {}", here.display());
        }
        return Ok(files);
    }

    let files = expand::expand_patterns(patterns)?;
    if files.is_empty() {
        bail!("no files matched {}", patterns.join(", "));
    }
    Ok(files)
}

/// Only the sources that produced a file in this very run, and to the trash
/// rather than gone, because an encode that looked fine can still disappoint.
fn trash_originals(report: &Report, config: &Config) {
    let originals: Vec<&Path> = report.encoded_inputs().collect();
    if originals.is_empty() {
        return;
    }
    if config.dry_run {
        println!(
            "
would move {} original(s) to the trash",
            originals.len()
        );
        return;
    }

    let mut moved = 0;
    for original in &originals {
        match trash::delete(original) {
            Ok(()) => moved += 1,
            Err(error) => eprintln!("squish: cannot trash {}: {error}", original.display()),
        }
    }
    if !config.quiet {
        println!(
            "moved {moved} of {} original(s) to the trash",
            originals.len()
        );
    }
}
