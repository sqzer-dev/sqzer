//! Feedback channels, ADR-0003: JSON Lines on stdout under `--json` and
//! nothing else there, ever; progress, warnings and errors on stderr.

use std::io::Write;
use std::sync::Mutex;

use anstyle::{AnsiColor, Style};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use sqzer::Output;
use sqzer::core::Registry;
use sqzer::core::codec::{Encoder, Format};
use sqzer::core::content::Content;
use sqzer::core::params::Resolved;
use sqzer::metrics::SearchReport;

use crate::cli::format_name;

/// What happened to one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The file was written.
    Written,
    /// Nothing was written and that is fine: the output would have been
    /// larger than the input.
    Skipped,
    /// A dry run: what would be written.
    Planned,
    /// This output failed. The run's exit code is 1.
    #[default]
    Failed,
}

/// One quality the search tried.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Trial {
    /// Abstract quality.
    pub quality: f32,
    /// SSIMULACRA2 score.
    pub score: f32,
}

/// One line of `--json`: one input, one output. Fields are omitted when
/// they do not apply, never null.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Record {
    /// Input path, or `-` for stdin.
    pub input: String,
    /// Output path, or `-` for stdout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// What happened.
    pub status: Status,
    /// Why nothing was written, for `skipped`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What went wrong, for `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Detected input format, as `-f` spells it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_format: Option<&'static str>,
    /// The input has more than one frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animated: Option<bool>,
    /// Width after orientation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Height after orientation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The input has an alpha channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha: Option<bool>,
    /// `photo` or `graphic`, the content class that picks the default
    /// format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<&'static str>,
    /// Input size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_bytes: Option<u64>,
    /// Output format, as `-f` spells it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<&'static str>,
    /// Backend crate that wrote the output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<&'static str>,
    /// Its tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Output size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// `output_bytes / input_bytes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    /// Abstract quality the encoder ran with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<f32>,
    /// The output is lossless.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lossless: Option<bool>,
    /// SSIMULACRA2 target that was searched for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<f32>,
    /// SSIMULACRA2 score of the output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// The score is at or above the target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reached: Option<bool>,
    /// The search hit the quality ceiling and still fell short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capped: Option<bool>,
    /// Encodes the search performed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u8>,
    /// Every trial in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trials: Option<Vec<Trial>>,
}

impl Record {
    /// A failure before anything was known about the input.
    pub fn failed(input: String, error: &dyn std::fmt::Display) -> Self {
        Self {
            input,
            status: Status::Failed,
            error: Some(error.to_string()),
            ..Self::default()
        }
    }

    /// Mark this record failed.
    pub fn fail(mut self, error: &dyn std::fmt::Display) -> Self {
        self.status = Status::Failed;
        self.error = Some(error.to_string());
        self
    }

    /// Fill in the encode result.
    pub fn with_output(mut self, out: &Output) -> Self {
        self.format = Some(format_name(out.format));
        self.backend = Some(out.backend);
        self.tier = Some(out.tier.to_string());
        self.content = Some(content_name(out.content));
        let len = out.bytes.len() as u64;
        self.output_bytes = Some(len);
        #[allow(clippy::cast_precision_loss)]
        if let Some(input) = self.input_bytes.filter(|&n| n > 0) {
            self.ratio = Some(len as f32 / input as f32);
        }
        match out.target {
            Resolved::Quality(q) => {
                self.quality = Some(q);
                self.lossless = Some(false);
            }
            Resolved::Lossless => self.lossless = Some(true),
        }
        if let Some(r) = &out.report {
            self.with_report(r);
        }
        self
    }

    fn with_report(&mut self, r: &SearchReport) {
        self.score = Some(r.score);
        self.reached = Some(r.reached);
        self.capped = Some(r.capped);
        self.iterations = Some(r.iterations);
        self.trials = Some(
            r.trials
                .iter()
                .map(|t| Trial {
                    quality: t.quality,
                    score: t.score,
                })
                .collect(),
        );
    }
}

/// `photo` / `graphic`.
pub fn content_name(c: Content) -> &'static str {
    match c {
        Content::Photo => "photo",
        Content::Graphic => "graphic",
    }
}

/// The feedback flags, resolved.
#[derive(Debug, Clone, Copy, Default)]
pub struct Feedback {
    /// Records go to stdout as JSON Lines.
    pub json: bool,
    /// Per-file lines and the summary on stderr.
    pub progress: bool,
    /// No warnings.
    pub quiet: bool,
    /// `-v` count.
    pub verbose: u8,
    /// Width of the longest input path, so the columns line up across
    /// workers that finish in any order.
    pub name_width: usize,
}

/// Running totals over every record of a run, for the summary line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Outputs written.
    pub written: u32,
    /// Outputs skipped as larger.
    pub skipped: u32,
    /// Outputs planned by a dry run.
    pub planned: u32,
    /// Outputs that failed.
    pub failed: u32,
    /// Input bytes behind the written outputs.
    pub input_bytes: u64,
    /// Bytes written.
    pub output_bytes: u64,
}

impl Tally {
    /// Count one record.
    pub fn add(&mut self, r: &Record) {
        match r.status {
            Status::Written => {
                self.written += 1;
                self.input_bytes += r.input_bytes.unwrap_or(0);
                self.output_bytes += r.output_bytes.unwrap_or(0);
            }
            Status::Skipped => self.skipped += 1,
            Status::Planned => self.planned += 1,
            Status::Failed => self.failed += 1,
        }
    }

    /// Two tallies as one.
    pub fn merge(self, o: Self) -> Self {
        Self {
            written: self.written + o.written,
            skipped: self.skipped + o.skipped,
            planned: self.planned + o.planned,
            failed: self.failed + o.failed,
            input_bytes: self.input_bytes + o.input_bytes,
            output_bytes: self.output_bytes + o.output_bytes,
        }
    }

    /// Records counted.
    pub fn total(&self) -> u32 {
        self.written + self.skipped + self.planned + self.failed
    }
}

// One palette, the same one clap uses for its own messages so an
// argument error and a file error look alike: bold red `error:`, bold
// yellow `warning:`, bold for the thing that was produced, dim for the
// context around it, green for bytes saved and yellow for bytes added.
const ERROR: Style = AnsiColor::Red.on_default().bold();
const WARNING: Style = AnsiColor::Yellow.on_default().bold();
const NAME: Style = Style::new().bold();
const DIM: Style = Style::new().dimmed();
const SMALLER: Style = AnsiColor::Green.on_default();
const LARGER: Style = AnsiColor::Yellow.on_default();
const PLANNED: Style = AnsiColor::Cyan.on_default();

fn paint(style: Style, text: &str) -> String {
    format!("{style}{text}{style:#}")
}

/// An error on stderr, in clap's style. A multi-line message keeps its
/// own indentation after the first line.
pub fn error_line(message: &str) {
    let mut err = anstream::stderr().lock();
    let _ = writeln!(err, "{} {message}", paint(ERROR, "error:"));
}

/// `dir/` dimmed, the file name in `name_style`, padded to `width`
/// visible characters.
fn path_cell(path: &str, name_style: Style, width: usize) -> String {
    let split = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let (dir, name) = path.split_at(split);
    let pad = width.saturating_sub(path.chars().count());
    format!(
        "{}{}{}",
        paint(DIM, dir),
        paint(name_style, name),
        " ".repeat(pad)
    )
}

/// Where one file is in the pipeline, for its line in the bar. Each
/// stage has its own colour: cyan while reading and decoding, magenta
/// while the encoder runs, green while writing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    /// Reading and probing the file.
    Read,
    /// Decoding.
    Decode,
    /// Encoding once at a known quality, or about to search.
    Encode,
    /// The search scored trial `n` of `max`.
    Trial {
        /// Position in the budget.
        n: u8,
        /// The budget.
        max: u8,
        /// Quality tried.
        quality: f32,
        /// Score it reached.
        score: f32,
    },
    /// Writing the output.
    Write,
}

impl From<sqzer::Progress> for Stage {
    fn from(p: sqzer::Progress) -> Self {
        match p {
            sqzer::Progress::Trial {
                n,
                max,
                quality,
                score,
            } => Self::Trial {
                n,
                max,
                quality,
                score,
            },
            // Stages the facade may add later show as plain encoding.
            _ => Self::Encode,
        }
    }
}

/// The bars: one overall bar plus one spinner per file in flight,
/// drawn on stderr and hidden when stderr is not a terminal.
#[derive(Debug)]
struct Bars {
    multi: MultiProgress,
    overall: ProgressBar,
}

impl Bars {
    fn new(total: u64) -> Self {
        let multi = MultiProgress::new();
        let overall = multi.add(ProgressBar::new(total));
        overall.set_style(
            ProgressStyle::with_template("{bar:48.green/237} {pos}/{len}  {elapsed}")
                .expect("static template")
                .progress_chars("━╸━"),
        );
        overall.enable_steady_tick(std::time::Duration::from_millis(100));
        Self { multi, overall }
    }
}

/// One file's spinner. Dropping it clears the spinner and advances the
/// overall bar.
#[derive(Debug)]
pub struct Worker<'a> {
    bars: &'a Bars,
    bar: ProgressBar,
}

impl Worker<'_> {
    /// Move the spinner to `stage`.
    pub fn stage(&self, stage: Stage) {
        let (color, message) = match stage {
            Stage::Read => ("cyan", "read".to_string()),
            Stage::Decode => ("cyan", "decode".to_string()),
            Stage::Encode => ("magenta", "encode".to_string()),
            Stage::Trial {
                n,
                max,
                quality,
                score,
            } => (
                "magenta",
                format!("search {n}/{max}  q{quality} -> {score:.1}"),
            ),
            Stage::Write => ("green", "write".to_string()),
        };
        self.bar.set_style(
            ProgressStyle::with_template(&format!(
                "{{spinner:.{color}}} {{prefix}}  {{msg:.{color}}}"
            ))
            .expect("static template")
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        self.bar.set_message(message);
    }
}

impl Drop for Worker<'_> {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
        self.bars.multi.remove(&self.bar);
        self.bars.overall.inc(1);
    }
}

/// The stderr and stdout writer shared by every worker.
#[derive(Debug)]
pub struct Printer {
    /// The flags.
    pub feedback: Feedback,
    bars: Option<Bars>,
    lock: Mutex<()>,
}

impl Printer {
    /// A printer with the given settings. With progress on, `total`
    /// inputs get an overall bar.
    pub fn new(feedback: Feedback, total: u64) -> Self {
        let bars = (feedback.progress && total > 0).then(|| Bars::new(total));
        Self {
            feedback,
            bars,
            lock: Mutex::new(()),
        }
    }

    /// A spinner for one file, or `None` when no bars are drawn.
    pub fn worker(&self, name: &str) -> Option<Worker<'_>> {
        let bars = self.bars.as_ref()?;
        let bar = bars
            .multi
            .insert_before(&bars.overall, ProgressBar::new_spinner());
        bar.set_prefix(name.to_string());
        bar.enable_steady_tick(std::time::Duration::from_millis(80));
        let worker = Worker { bars, bar };
        worker.stage(Stage::Read);
        Some(worker)
    }

    /// Run `f` with the bars lifted off the screen, so a line printed
    /// under them stays put.
    fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        match &self.bars {
            Some(b) => b.multi.suspend(f),
            None => f(),
        }
    }

    /// Clear the overall bar once the run is over.
    pub fn finish(&self) {
        if let Some(b) = &self.bars {
            b.overall.finish_and_clear();
        }
    }

    /// An error line. Always printed.
    pub fn error(&self, message: &str) {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.suspend(|| error_line(message));
    }

    /// One finished output: the JSON line if `--json`, the human line if
    /// progress is on, the error line if it failed. `details` are the
    /// `-vv` lines.
    pub fn record(&self, r: &Record, details: &[String]) {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.suspend(|| self.write_record(r, details));
    }

    fn write_record(&self, r: &Record, details: &[String]) {
        let fb = self.feedback;
        if fb.json {
            let mut out = std::io::stdout().lock();
            if let Ok(line) = serde_json::to_string(r) {
                let _ = writeln!(out, "{line}");
            }
            let _ = out.flush();
        }
        let mut err = anstream::stderr().lock();
        match r.status {
            Status::Failed => {
                let where_ = match &r.output {
                    Some(o) => format!("{} -> {o}", r.input),
                    None => r.input.clone(),
                };
                let _ = writeln!(
                    err,
                    "{} {}: {}",
                    paint(ERROR, "error:"),
                    paint(NAME, &where_),
                    r.error.as_deref().unwrap_or("failed")
                );
            }
            _ if fb.progress => {
                let _ = writeln!(err, "{}", self.human_line(r));
            }
            _ => {}
        }
        if fb.verbose >= 1
            && let Some(trials) = &r.trials
        {
            let list: Vec<String> = trials
                .iter()
                .map(|t| format!("q{} {:.2}", t.quality, t.score))
                .collect();
            let _ = writeln!(
                err,
                "  {}",
                paint(DIM, &format!("trials: {}", list.join(", ")))
            );
        }
        if fb.verbose >= 2 {
            for d in details {
                let _ = writeln!(err, "  {}", paint(DIM, d));
            }
        }
        if let (Some(false), Some(score), Some(target)) = (r.reached, r.score, r.target)
            && !fb.quiet
            && r.status == Status::Written
        {
            let q = r.quality.map_or(String::new(), |q| format!(" at q{q}"));
            let _ = writeln!(
                err,
                "{} {} -> {}: target {target} not reached, best score {score:.1}{q}",
                paint(WARNING, "warning:"),
                r.input,
                r.output.as_deref().unwrap_or("-")
            );
        }
    }

    /// The closing line of a run, when progress is on and there was more
    /// than one output to sum up.
    pub fn summary(&self, t: &Tally, elapsed: std::time::Duration) {
        if !self.feedback.progress || t.total() < 2 {
            return;
        }
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.suspend(|| Self::write_summary(t, elapsed));
    }

    fn write_summary(t: &Tally, elapsed: std::time::Duration) {
        let mut counts = Vec::new();
        let mut count = |n: u32, what: &str, style: Option<Style>| {
            if n > 0 {
                let text = format!("{n} {what}");
                counts.push(style.map_or(text.clone(), |s| paint(s, &text)));
            }
        };
        count(t.written, "written", None);
        count(t.planned, "planned", Some(PLANNED));
        count(t.skipped, "skipped", Some(LARGER));
        count(t.failed, "failed", Some(ERROR));
        let sizes = if t.written > 0 {
            format!(
                "   {} -> {} {}",
                fmt_bytes(t.input_bytes),
                paint(NAME, &fmt_bytes(t.output_bytes)),
                change_cell(t.input_bytes, t.output_bytes)
            )
        } else {
            String::new()
        };
        let mut err = anstream::stderr().lock();
        let _ = writeln!(
            err,
            "\n{}{sizes}   {}",
            counts.join(", "),
            paint(DIM, &format!("{:.1} s", elapsed.as_secs_f64()))
        );
    }

    fn human_line(&self, r: &Record) -> String {
        let w = self.feedback.name_width;
        let arrow = paint(DIM, "->");
        let input = path_cell(&r.input, Style::new(), w);
        match r.status {
            Status::Written => {
                let output = path_cell(r.output.as_deref().unwrap_or("-"), NAME, w);
                let sizes = match (r.input_bytes, r.output_bytes) {
                    (Some(i), Some(o)) => format!(
                        "  {:>8} {arrow} {:>8}  {}",
                        fmt_bytes(i),
                        fmt_bytes(o),
                        change_cell(i, o)
                    ),
                    (_, Some(o)) => format!("  {:>8}", fmt_bytes(o)),
                    _ => String::new(),
                };
                let how = match (r.lossless, r.quality, r.score) {
                    (Some(true), _, _) => "lossless".to_string(),
                    (_, Some(q), Some(s)) => format!("q{q} s{s:.1}"),
                    (_, Some(q), None) => format!("q{q}"),
                    _ => String::new(),
                };
                format!(
                    "{input} {arrow} {output}{sizes}  {}",
                    paint(DIM, &format!("{} {how}", r.format.unwrap_or("")))
                )
            }
            Status::Skipped => {
                let output = path_cell(r.output.as_deref().unwrap_or("-"), Style::new(), w);
                format!(
                    "{input} {arrow} {output}  {}  {}",
                    paint(LARGER, "skipped"),
                    paint(DIM, r.reason.as_deref().unwrap_or(""))
                )
            }
            Status::Planned => {
                let dims = match (r.width, r.height) {
                    (Some(w), Some(h)) => format!("{w}x{h}"),
                    _ => String::new(),
                };
                let alpha = if r.alpha == Some(true) { " alpha" } else { "" };
                let output = path_cell(r.output.as_deref().unwrap_or("-"), NAME, 0);
                format!(
                    "{input}  {}  {} {} {arrow} {output} {}",
                    paint(PLANNED, &format!("{dims}{alpha}")),
                    paint(DIM, r.content.unwrap_or("")),
                    paint(DIM, r.input_format.unwrap_or("?")),
                    paint(DIM, &format!("({})", r.format.unwrap_or("?")))
                )
            }
            Status::Failed => unreachable!("failures are rendered separately"),
        }
    }
}

/// `-49%` in green, `+12%` in yellow, right-aligned to five columns.
fn change_cell(input: u64, output: u64) -> String {
    let text = format!("{:>5}", fmt_change(input, output));
    if output <= input {
        paint(SMALLER, &text)
    } else {
        paint(LARGER, &text)
    }
}

/// `673 B`, `45.6 KB`, `1.2 MB`.
pub fn fmt_bytes(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    if n < 1000 {
        format!("{n} B")
    } else if n < 1_000_000 {
        format!("{:.1} KB", f / 1e3)
    } else if n < 1_000_000_000 {
        format!("{:.1} MB", f / 1e6)
    } else {
        format!("{:.2} GB", f / 1e9)
    }
}

/// `-63%` or `+12%`.
pub fn fmt_change(input: u64, output: u64) -> String {
    if input == 0 {
        return "n/a".into();
    }
    #[allow(clippy::cast_precision_loss)]
    let pct = (output as f64 - input as f64) / input as f64 * 100.0;
    format!("{pct:+.0}%")
}

/// Which mode an encoder was wanted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Any encoder.
    Any,
    /// A lossy one.
    Lossy,
    /// A lossless one.
    Lossless,
}

/// The ADR-0003 rendering of an unavailable encoder: what was wanted,
/// what this build has instead, and which feature or build would add it.
pub fn render_unavailable(format: Format, need: Need, registry: &Registry) -> String {
    let have: Vec<String> = registry
        .encoders()
        .map(|e| {
            let c = e.caps();
            let mode = match (c.lossy, c.lossless) {
                (true, _) => "",
                (false, true) => " lossless",
                (false, false) => " (no mode)",
            };
            format!("{}{mode} ({}, {})", c.format, c.name, c.tier)
        })
        .collect();
    let mut lines = Vec::new();
    let for_what = match need {
        Need::Any => String::new(),
        Need::Lossy => " for lossy output".into(),
        Need::Lossless => " for lossless output".into(),
    };
    lines.push(format!("no {format} encoder in this build{for_what}"));
    if have.is_empty() {
        lines.push("  this build encodes nothing".to_string());
    } else {
        lines.push(format!("  this build encodes: {}", have.join(", ")));
    }
    let existing = registry.encoder(format).ok().map(Encoder::caps);
    // A native feature can add a lossy mode to a format whose portable
    // encoder is lossless only (WebP). None adds a lossless mode, so a
    // lossless request with an existing encoder names no feature.
    let native: Vec<&str> = format
        .encoder_features()
        .iter()
        .copied()
        .filter(|f| existing.is_none() || (need != Need::Lossless && f.starts_with("native-")))
        .collect();
    let mode = match need {
        Need::Any => format!("{format}"),
        Need::Lossy => format!("lossy {format}"),
        Need::Lossless => format!("lossless {format}"),
    };
    match (existing, native.is_empty()) {
        (Some(c), true) => lines.push(format!(
            "  {} writes {} only; no build offers {mode}",
            c.name,
            if c.lossy { "lossy" } else { "lossless" }
        )),
        (Some(c), false) => lines.push(format!(
            "  {} writes {} only; {mode} needs the `{}` feature, or a native build from the releases page",
            c.name,
            if c.lossy { "lossy" } else { "lossless" },
            native.join("` or `")
        )),
        (None, true) => lines.push(format!("  no build offers a {format} encoder")),
        (None, false) => lines.push(format!(
            "  {mode} needs the `{}` feature, or a native build from the releases page",
            native.join("` or `")
        )),
    }
    if need != Need::Any && existing.is_some() {
        let fix = match need {
            Need::Lossy => "drop -q for the lossless output this build can write",
            _ => "drop --lossless for lossy output",
        };
        lines.push(format!("  or {fix}"));
    }
    lines.push("  run `sqzer --list-codecs` for the full list".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_changes_read_well() {
        assert_eq!(fmt_bytes(673), "673 B");
        assert_eq!(fmt_bytes(45_600), "45.6 KB");
        assert_eq!(fmt_bytes(1_234_567), "1.2 MB");
        assert_eq!(fmt_change(1000, 370), "-63%");
        assert_eq!(fmt_change(1000, 1120), "+12%");
        assert_eq!(fmt_change(0, 5), "n/a");
    }

    #[test]
    fn tally_counts_and_sums_written_bytes() {
        let mut t = Tally::default();
        t.add(&Record {
            status: Status::Written,
            input_bytes: Some(1000),
            output_bytes: Some(400),
            ..Record::default()
        });
        t.add(&Record {
            status: Status::Skipped,
            input_bytes: Some(1000),
            output_bytes: Some(1200),
            ..Record::default()
        });
        t.add(&Record::failed("x".into(), &"boom"));
        let t = t.merge(Tally {
            planned: 2,
            ..Tally::default()
        });
        assert_eq!((t.written, t.skipped, t.planned, t.failed), (1, 1, 2, 1));
        assert_eq!((t.input_bytes, t.output_bytes), (1000, 400));
        assert_eq!(t.total(), 5);
    }

    #[test]
    fn path_cells_pad_to_visible_width() {
        let cell = path_cell("dir/sub/name.png", Style::new(), 20);
        assert!(cell.ends_with("    "), "{cell:?}");
        assert!(cell.contains("name.png"));
        let cell = path_cell("图片.png", Style::new(), 8);
        assert!(
            cell.ends_with("  "),
            "padding counts characters, not bytes: {cell:?}"
        );
    }

    #[test]
    fn json_omits_what_does_not_apply() {
        let r = Record::failed("x.png".into(), &"boom");
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(v["error"], "boom");
        assert!(v.get("output").is_none());
        assert!(v.get("score").is_none());
    }

    #[cfg(feature = "portable")]
    #[test]
    fn unavailable_renderer_names_the_fix() {
        let reg = sqzer::codecs::registry();
        let text = render_unavailable(Format::Jxl, Need::Any, &reg);
        assert!(
            text.starts_with("no JPEG XL encoder in this build"),
            "{text}"
        );
        assert!(text.contains("mozjpeg-rs"), "{text}");
        assert!(text.contains("`native-jxl`"), "{text}");
        assert!(text.contains("--list-codecs"), "{text}");
        let text = render_unavailable(Format::WebP, Need::Lossy, &reg);
        assert!(text.contains("for lossy output"), "{text}");
        assert!(text.contains("`native-webp`"), "{text}");
        assert!(text.contains("drop -q"), "{text}");
        let text = render_unavailable(Format::Jpeg, Need::Lossless, &reg);
        assert!(text.contains("no build offers lossless JPEG"), "{text}");
    }
}
