use std::io::{IsTerminal, Write};

const REWIND: &str = "\u{1b}[1A\u{1b}[2K";
const CLEAR_LINE_END: &str = "\u{1b}[K";
const NAME_COLUMN_MAX: usize = 40;
const BAR_WIDTH: usize = 24;

/// One snapshot of `-progress` output.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Tick {
    pub out_time_us: u64,
    pub speed: f32,
    pub done: bool,
}

/// Folds one `key=value` line into `tick`; true once a whole block has arrived.
pub fn apply(line: &str, tick: &mut Tick) -> bool {
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    let value = value.trim();
    match key.trim() {
        "out_time_us" | "out_time_ms" => {
            if let Ok(us) = value.parse() {
                tick.out_time_us = us;
            }
        }
        "speed" => {
            if let Ok(speed) = value.trim_end_matches('x').parse() {
                tick.speed = speed;
            }
        }
        "progress" => {
            tick.done = value == "end";
            return true;
        }
        _ => {}
    }
    false
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Pending,
    Active {
        tick: Tick,
        duration: Option<f64>,
        codecs: String,
    },
    Done(String),
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    pub profile: Option<String>,
    pub status: Status,
    pub notes: Vec<String>,
}

impl Row {
    pub fn new(name: String) -> Self {
        Row {
            name,
            profile: None,
            status: Status::Pending,
            notes: Vec::new(),
        }
    }

    pub fn is_settled(&self) -> bool {
        matches!(
            self.status,
            Status::Done(_) | Status::Skipped(_) | Status::Failed(_)
        )
    }

    fn is_active(&self) -> bool {
        matches!(self.status, Status::Active { .. })
    }

    fn lines(&self, widths: Widths) -> Vec<String> {
        let name = truncate(&self.name, widths.name);
        let profile = self.profile.as_deref().unwrap_or("");
        let status = self.status.text();
        let mut lines = vec![if widths.profile == 0 {
            format!("  {name:<nw$}  {status}", nw = widths.name)
        } else {
            format!(
                "  {name:<nw$}  {profile:<pw$}  {status}",
                nw = widths.name,
                pw = widths.profile
            )
        }];
        lines.extend(self.notes.iter().map(|note| format!("      {note}")));
        lines
    }
}

#[derive(Debug, Clone, Copy)]
struct Widths {
    name: usize,
    profile: usize,
}

impl Status {
    pub fn text(&self) -> String {
        match self {
            Status::Pending => "pending".to_owned(),
            Status::Done(text) | Status::Skipped(text) => text.clone(),
            Status::Failed(text) => format!("err: {text}"),
            Status::Active {
                tick,
                duration,
                codecs,
            } => {
                let speed = match tick.speed {
                    s if s > 0.0 => format!("{s:.1}x"),
                    _ => "--".to_owned(),
                };
                let position = match fraction(*duration, tick.out_time_us) {
                    Some(done) => format!("{:>3}%", (done * 100.0).round() as u32),
                    None => format!("{:>4}", clock(tick.out_time_us)),
                };
                let filled = fraction(*duration, tick.out_time_us).unwrap_or(0.0);
                format!("{}  {position}  {speed:>5}  {codecs}", bar(filled))
            }
        }
    }
}

fn fraction(duration: Option<f64>, out_time_us: u64) -> Option<f64> {
    let duration = duration.filter(|d| *d > 0.0)?;
    Some((out_time_us as f64 / 1_000_000.0 / duration).clamp(0.0, 1.0))
}

fn bar(fraction: f64) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * BAR_WIDTH as f64).round() as usize).min(BAR_WIDTH);
    format!("[{}{}]", "=".repeat(filled), " ".repeat(BAR_WIDTH - filled))
}

fn clock(micros: u64) -> String {
    let total = micros / 1_000_000;
    format!("{}:{:02}", total / 60, total % 60)
}

fn truncate(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_owned();
    }
    chars[..width.saturating_sub(1)].iter().collect::<String>() + "~"
}

/// The whole file list. When it does not fit, done and pending share the space
/// left over by the running files, half each, and either grows into what the
/// other does not use.
pub fn render(rows: &[Row], max_lines: usize) -> Vec<String> {
    if rows.is_empty() || max_lines == 0 {
        return Vec::new();
    }
    let widths = Widths {
        name: column(rows.iter().map(|row| row.name.chars().count())),
        profile: column(
            rows.iter()
                .map(|row| row.profile.as_deref().unwrap_or("").chars().count()),
        ),
    };
    let blocks: Vec<Vec<String>> = rows.iter().map(|row| row.lines(widths)).collect();
    let cost = |index: usize| blocks[index].len();

    if blocks.iter().map(Vec::len).sum::<usize>() <= max_lines {
        return blocks.concat();
    }

    let group = |keep: fn(&Row) -> bool| -> Vec<usize> {
        (0..rows.len()).filter(|i| keep(&rows[*i])).collect()
    };
    let done = group(Row::is_settled);
    let active = group(Row::is_active);
    let pending = group(|row| matches!(row.status, Status::Pending));

    let total = |group: &[usize]| group.iter().copied().map(cost).sum::<usize>();
    let (shown_active, _) = take(&active, &cost, max_lines.saturating_sub(2).max(1), false);
    let rest = max_lines - total(&shown_active);

    let (done_budget, pending_budget) = split(rest, total(&done), total(&pending));
    let (shown_done, hidden_done) = take(&done, &cost, done_budget, true);
    let (shown_pending, hidden_pending) = take(&pending, &cost, pending_budget, false);

    let mut out = Vec::with_capacity(max_lines);
    if hidden_done > 0 {
        out.push(format!("  ... {hidden_done} done"));
    }
    for index in shown_done
        .iter()
        .chain(&shown_active)
        .chain(&shown_pending)
        .copied()
    {
        out.extend(blocks[index].iter().cloned());
    }
    if hidden_pending > 0 {
        out.push(format!("  ... {hidden_pending} pending"));
    }
    out
}

/// Equal shares, with whatever one side leaves over handed to the other.
fn split(budget: usize, first_need: usize, second_need: usize) -> (usize, usize) {
    let half = (budget / 2).min(first_need);
    let second = (budget - half).min(second_need);
    ((budget - second).min(first_need), second)
}

/// Fits as many whole rows as the budget allows, keeping one line for the
/// "... n more" marker when something has to go.
fn take(
    indices: &[usize],
    cost: &dyn Fn(usize) -> usize,
    budget: usize,
    from_end: bool,
) -> (Vec<usize>, usize) {
    if indices.iter().copied().map(cost).sum::<usize>() <= budget {
        return (indices.to_vec(), 0);
    }
    let usable = budget.saturating_sub(1);
    let mut taken = Vec::new();
    let mut used = 0;
    let ordered: Vec<usize> = if from_end {
        indices.iter().rev().copied().collect()
    } else {
        indices.to_vec()
    };
    for index in ordered {
        let lines = cost(index);
        if used + lines > usable {
            break;
        }
        used += lines;
        taken.push(index);
    }
    if from_end {
        taken.reverse();
    }
    let hidden = indices.len() - taken.len();
    (taken, hidden)
}

fn column(widths: impl Iterator<Item = usize>) -> usize {
    widths.max().unwrap_or(0).min(NAME_COLUMN_MAX)
}

/// Lines the status list may take, leaving room for what comes after it.
pub fn viewport() -> usize {
    terminal_size::terminal_size()
        .map(|(_, terminal_size::Height(rows))| usize::from(rows).saturating_sub(3))
        .unwrap_or(20)
        .max(3)
}

fn terminal_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(columns), _)| usize::from(columns).saturating_sub(1))
        .unwrap_or(120)
        .max(20)
}

fn up(lines: usize) -> String {
    match lines {
        0 => String::new(),
        n => format!("\u{1b}[{n}A"),
    }
}

/// One write per frame, so no half-drawn state ever reaches the screen.
fn write_frame(out: &mut impl Write, frame: &str) -> std::io::Result<()> {
    if frame.is_empty() {
        return Ok(());
    }
    out.write_all(frame.as_bytes())?;
    out.flush()
}

/// Keeps a block of lines up to date by overwriting the ones that changed.
/// Erasing the block first would leave a blank frame on screen, which reads as
/// flicker, so nothing is ever erased ahead of the text replacing it.
#[derive(Debug, Default)]
pub struct Display {
    last: Vec<String>,
}

impl Display {
    pub fn is_live() -> bool {
        std::io::stdout().is_terminal()
    }

    pub fn clear(&mut self) {
        let _ = self.clear_to(&mut std::io::stdout().lock());
    }

    /// A line wider than the terminal would wrap and throw the row arithmetic
    /// off by one, so every line is cut to fit first.
    pub fn draw(&mut self, rows: &[String]) {
        let width = terminal_width();
        let fitted: Vec<String> = rows.iter().map(|row| truncate(row, width)).collect();
        let _ = self.draw_to(&mut std::io::stdout().lock(), &fitted);
    }

    fn clear_to(&mut self, out: &mut impl Write) -> std::io::Result<()> {
        let mut frame = String::new();
        for _ in 0..self.last.len() {
            frame.push_str(REWIND);
        }
        self.last.clear();
        write_frame(out, &frame)
    }

    fn draw_to(&mut self, out: &mut impl Write, rows: &[String]) -> std::io::Result<()> {
        let mut frame = String::new();
        if self.last.len() == rows.len() {
            if rows.iter().eq(self.last.iter()) {
                return Ok(());
            }
            frame.push_str(&up(rows.len()));
            for (row, previous) in rows.iter().zip(&self.last) {
                if row != previous {
                    frame.push('\r');
                    frame.push_str(row);
                    frame.push_str(CLEAR_LINE_END);
                }
                frame.push('\n');
            }
        } else {
            frame.push_str(&up(self.last.len()));
            for row in rows {
                frame.push('\r');
                frame.push_str(row);
                frame.push_str(CLEAR_LINE_END);
                frame.push('\n');
            }
            for _ in rows.len()..self.last.len() {
                frame.push('\r');
                frame.push_str(CLEAR_LINE_END);
                frame.push('\n');
            }
            if rows.len() < self.last.len() {
                frame.push_str(&up(self.last.len() - rows.len()));
            }
        }

        self.last = rows.to_vec();
        write_frame(out, &frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> (Tick, usize) {
        let mut tick = Tick::default();
        let mut blocks = 0;
        for line in lines {
            if apply(line, &mut tick) {
                blocks += 1;
            }
        }
        (tick, blocks)
    }

    fn active(name: &str, seconds: f64, of: f64) -> Row {
        Row {
            name: name.to_owned(),
            profile: Some("phone".into()),
            status: Status::Active {
                tick: Tick {
                    out_time_us: (seconds * 1_000_000.0) as u64,
                    speed: 0.4,
                    done: false,
                },
                duration: Some(of),
                codecs: "hevc aac (apple_apac dropped)".into(),
            },
            notes: Vec::new(),
        }
    }

    fn settled(name: &str) -> Row {
        Row {
            status: Status::Done("41.9 MiB -> 5.5 MiB (-86.9%)".into()),
            profile: Some("phone".into()),
            ..Row::new(name.to_owned())
        }
    }

    #[test]
    fn a_progress_block_is_folded_into_one_tick() {
        let (tick, blocks) = feed(&[
            "frame=120",
            "fps=60.0",
            "out_time_us=2000000",
            "speed=3.4x",
            "progress=continue",
        ]);
        assert_eq!(blocks, 1);
        assert_eq!(tick.out_time_us, 2_000_000);
        assert_eq!(tick.speed, 3.4);
        assert!(!tick.done);
    }

    #[test]
    fn the_last_block_is_marked_done() {
        let (tick, _) = feed(&["out_time_us=9000000", "progress=end"]);
        assert!(tick.done);
        assert_eq!(tick.out_time_us, 9_000_000);
    }

    #[test]
    fn unparsable_values_are_ignored() {
        let (tick, blocks) = feed(&["out_time_us=N/A", "speed=N/A", "garbage", "bitrate=N/A"]);
        assert_eq!(blocks, 0);
        assert_eq!(tick, Tick::default());
    }

    #[test]
    fn out_time_ms_is_accepted_too() {
        let (tick, _) = feed(&["out_time_ms=1500000", "progress=continue"]);
        assert_eq!(tick.out_time_us, 1_500_000);
    }

    #[test]
    fn the_bar_fills_with_the_fraction() {
        assert!(bar(0.0).starts_with("[    "));
        assert_eq!(bar(1.0), format!("[{}]", "=".repeat(BAR_WIDTH)));
        assert_eq!(bar(9.9), bar(1.0));
    }

    #[test]
    fn every_status_has_its_own_shape() {
        let rows = vec![
            Row::new("queued.mov".into()),
            active("running.mov", 83.0, 100.0),
            Row {
                status: Status::Done("79.3 MiB -> 91.3 MiB (+15.1%)".into()),
                ..Row::new("done.mov".into())
            },
            Row {
                status: Status::Failed("moov atom not found".into()),
                ..Row::new("broken.mov".into())
            },
        ];
        let lines = render(&rows, 20);
        assert!(lines[0].contains("pending"), "{}", lines[0]);
        assert!(lines[1].contains("83%"), "{}", lines[1]);
        assert!(lines[1].contains("0.4x"), "{}", lines[1]);
        assert!(
            lines[1].contains("hevc aac (apple_apac dropped)"),
            "{}",
            lines[1]
        );
        assert!(lines[2].contains("+15.1%"), "{}", lines[2]);
        assert!(
            lines[3].contains("err: moov atom not found"),
            "{}",
            lines[3]
        );
    }

    #[test]
    fn names_line_up_in_one_column() {
        let rows = vec![Row::new("a.mov".into()), Row::new("much-longer.mov".into())];
        let lines = render(&rows, 20);
        let first = lines[0].find("pending").unwrap();
        let second = lines[1].find("pending").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn notes_sit_under_their_file_and_are_indented_further() {
        let rows = vec![Row {
            notes: vec!["cannot remove the half-written output".into()],
            ..Row::new("clip.mov".into())
        }];
        let lines = render(&rows, 20);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("      cannot remove"), "{}", lines[1]);
    }

    #[test]
    fn the_profile_is_a_column_of_its_own() {
        let rows = vec![
            active("running.mov", 1.0, 10.0),
            Row::new("queued.mov".into()),
        ];
        let lines = render(&rows, 20);
        let profile_at = lines[0].find("phone").unwrap();
        let status_at = lines[0].find('[').unwrap();
        assert!(profile_at < status_at, "{}", lines[0]);
        assert_eq!(lines[1].find("pending").unwrap(), status_at);
    }

    #[test]
    fn done_and_pending_split_what_the_running_files_leave() {
        let mut rows: Vec<Row> = (0..50).map(|i| settled(&format!("file{i}.mov"))).collect();
        rows[30] = active("file30.mov", 1.0, 10.0);
        for (i, row) in rows.iter_mut().enumerate().skip(31) {
            *row = Row::new(format!("file{i}.mov"));
        }

        let lines = render(&rows, 11);
        assert!(lines.len() <= 11, "{lines:#?}");
        assert_eq!(lines[0], "  ... 26 done");
        assert!(lines.last().unwrap().ends_with("pending"), "{lines:#?}");

        let done = lines.iter().filter(|l| l.contains("-86.9%")).count();
        let waiting = lines
            .iter()
            .filter(|l| l.ends_with("pending") && !l.starts_with("  ..."))
            .count();
        assert_eq!(done, 4, "{lines:#?}");
        assert_eq!(waiting, 4, "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("file30.mov")));
    }

    #[test]
    fn the_newest_done_files_are_the_ones_kept() {
        let mut rows: Vec<Row> = (0..20).map(|i| settled(&format!("file{i}.mov"))).collect();
        rows[19] = active("file19.mov", 1.0, 10.0);

        let lines = render(&rows, 6);
        assert_eq!(lines[0], "  ... 15 done");
        assert!(lines.iter().any(|l| l.contains("file18.mov")), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("file0.mov")), "{lines:#?}");
    }

    #[test]
    fn a_group_that_needs_less_leaves_the_rest_to_the_other() {
        let mut rows: Vec<Row> = (0..30).map(|i| settled(&format!("file{i}.mov"))).collect();
        rows[28] = active("file28.mov", 1.0, 10.0);
        rows[29] = Row::new("file29.mov".into());

        let lines = render(&rows, 10);
        let done = lines.iter().filter(|l| l.contains("-86.9%")).count();
        assert_eq!(done, 7, "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("file29.mov")), "{lines:#?}");
    }

    #[test]
    fn halves_grow_into_each_other() {
        assert_eq!(split(10, 20, 20), (5, 5));
        assert_eq!(split(10, 2, 20), (2, 8));
        assert_eq!(split(10, 20, 3), (7, 3));
        assert_eq!(split(10, 1, 1), (1, 1));
    }

    #[test]
    fn a_short_list_is_shown_whole() {
        let rows: Vec<Row> = (0..4).map(|i| Row::new(format!("file{i}.mov"))).collect();
        assert_eq!(render(&rows, 10).len(), 4);
    }

    #[test]
    fn without_a_duration_the_row_shows_elapsed_media_time() {
        let row = Row {
            status: Status::Active {
                tick: Tick {
                    out_time_us: 125_000_000,
                    speed: 0.0,
                    done: false,
                },
                duration: None,
                codecs: "h264".into(),
            },
            ..Row::new("clip.mov".into())
        };
        let line = &render(&[row], 5)[0];
        assert!(line.contains("2:05"), "{line}");
        assert!(line.contains("--"), "{line}");
    }

    #[test]
    fn long_names_are_cut_to_the_column() {
        assert_eq!(truncate("short.mov", 20), "short.mov");
        assert_eq!(truncate("a-very-long-name.mov", 10), "a-very-lo~");
    }

    #[test]
    fn a_row_wider_than_the_terminal_is_cut_so_it_cannot_wrap() {
        let row = active("some-quite-long-name.mov", 50.0, 100.0);
        let line = &render(&[row], 5)[0];
        assert!(line.chars().count() > 40);
        assert_eq!(truncate(line, 40).chars().count(), 40);
    }

    #[test]
    fn the_first_frame_just_writes_the_lines() {
        let mut display = Display::default();
        let mut out: Vec<u8> = Vec::new();
        display
            .draw_to(&mut out, &["one".into(), "two".into()])
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("\rone{CLEAR_LINE_END}\n\rtwo{CLEAR_LINE_END}\n")
        );
    }

    #[test]
    fn only_the_changed_lines_are_written_again() {
        let mut display = Display::default();
        display
            .draw_to(
                &mut Vec::new(),
                &["one".into(), "two".into(), "three".into()],
            )
            .unwrap();

        let mut out: Vec<u8> = Vec::new();
        display
            .draw_to(&mut out, &["one".into(), "CHANGED".into(), "three".into()])
            .unwrap();

        // Up three lines, past the first, rewrite the second, past the third.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("\u{1b}[3A\n\rCHANGED{CLEAR_LINE_END}\n\n")
        );
    }

    #[test]
    fn an_unchanged_frame_writes_nothing_at_all() {
        let mut display = Display::default();
        let rows = vec!["one".to_owned(), "two".to_owned()];
        display.draw_to(&mut Vec::new(), &rows).unwrap();

        let mut out: Vec<u8> = Vec::new();
        display.draw_to(&mut out, &rows).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_frame_that_lost_lines_erases_the_leftovers() {
        let mut display = Display::default();
        display
            .draw_to(&mut Vec::new(), &["one".into(), "two".into()])
            .unwrap();

        let mut out: Vec<u8> = Vec::new();
        display.draw_to(&mut out, &["only".into()]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("\u{1b}[2A\ronly{CLEAR_LINE_END}\n\r{CLEAR_LINE_END}\n\u{1b}[1A")
        );
    }

    #[test]
    fn clearing_rewinds_every_line_it_drew() {
        let mut display = Display::default();
        display
            .draw_to(&mut Vec::new(), &["one".into(), "two".into()])
            .unwrap();

        let mut out: Vec<u8> = Vec::new();
        display.clear_to(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), format!("{REWIND}{REWIND}"));

        let mut out: Vec<u8> = Vec::new();
        display.clear_to(&mut out).unwrap();
        assert!(out.is_empty());
    }
}
