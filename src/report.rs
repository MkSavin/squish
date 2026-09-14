use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum Outcome {
    Encoded { input: u64, output: u64 },
    Skipped { reason: String },
    Failed { error: String },
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub outcome: Outcome,
    pub output: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Report {
    entries: Vec<Entry>,
}

impl Report {
    pub fn push(&mut self, path: &Path, outcome: Outcome, output: Option<PathBuf>) {
        self.entries.push(Entry {
            path: path.to_path_buf(),
            outcome,
            output,
        });
    }

    pub fn outputs(&self) -> impl Iterator<Item = &Path> {
        self.encoded()
            .map(|entry| entry.output.as_deref().expect("encoded files have one"))
    }

    /// Sources that produced a file in this run, and only those.
    pub fn encoded_inputs(&self) -> impl Iterator<Item = &Path> {
        self.encoded().map(|entry| entry.path.as_path())
    }

    fn encoded(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| {
            matches!(entry.outcome, Outcome::Encoded { .. }) && entry.output.is_some()
        })
    }

    pub fn has_failures(&self) -> bool {
        self.entries
            .iter()
            .any(|e| matches!(e.outcome, Outcome::Failed { .. }))
    }

    pub fn print(&self) {
        if self.entries.is_empty() {
            return;
        }
        let width = self
            .entries
            .iter()
            .map(|e| file_name(&e.path).chars().count())
            .max()
            .unwrap_or(0);

        println!("\nSummary");
        let (mut encoded, mut skipped, mut failed) = (0u32, 0u32, 0u32);
        let (mut total_in, mut total_out) = (0u64, 0u64);

        for entry in &self.entries {
            let name = file_name(&entry.path);
            match &entry.outcome {
                Outcome::Encoded { input, output } => {
                    encoded += 1;
                    total_in += input;
                    total_out += output;
                    let grew = if output > input { "  larger, kept" } else { "" };
                    println!(
                        "  {name:width$}  {:>10} -> {:>10}  {:>7}{grew}",
                        human_size(*input),
                        human_size(*output),
                        change(*input, *output),
                    );
                }
                Outcome::Skipped { reason } => {
                    skipped += 1;
                    println!("  {name:width$}  skipped: {reason}");
                }
                Outcome::Failed { error } => {
                    failed += 1;
                    println!("  {name:width$}  failed: {error}");
                }
            }
        }

        println!("\n{encoded} encoded, {skipped} skipped, {failed} failed");
        if encoded > 0 {
            let saved = total_in.saturating_sub(total_out);
            println!(
                "{} -> {} ({} saved, {})",
                human_size(total_in),
                human_size(total_out),
                human_size(saved),
                change(total_in, total_out)
            );
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Signed size change, negative when the file got smaller.
pub fn change(input: u64, output: u64) -> String {
    if input == 0 {
        return "n/a".into();
    }
    let ratio = (output as f64 - input as f64) / input as f64 * 100.0;
    format!("{ratio:+.1}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn change_is_signed() {
        assert_eq!(change(1000, 400), "-60.0%");
        assert_eq!(change(1000, 1250), "+25.0%");
        assert_eq!(change(1000, 1000), "+0.0%");
        assert_eq!(change(0, 10), "n/a");
    }

    #[test]
    fn failures_are_tracked() {
        let mut report = Report::default();
        report.push(
            Path::new("a.mov"),
            Outcome::Encoded {
                input: 10,
                output: 5,
            },
            Some(PathBuf::from("a.squish.mp4")),
        );
        assert!(!report.has_failures());
        report.push(
            Path::new("b.mov"),
            Outcome::Failed {
                error: "boom".into(),
            },
            None,
        );
        assert!(report.has_failures());

        assert_eq!(
            report.encoded_inputs().collect::<Vec<_>>(),
            [Path::new("a.mov")]
        );
        assert_eq!(
            report.outputs().collect::<Vec<_>>(),
            [Path::new("a.squish.mp4")]
        );
    }
}
