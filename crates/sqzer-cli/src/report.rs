//! Feedback channels, ADR-0003: JSON Lines on stdout under `--json` and
//! nothing else there, ever; progress, warnings and errors on stderr.

use std::io::Write;
use std::sync::Mutex;

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
#[allow(clippy::struct_excessive_bools)]
pub struct Feedback {
    /// Records go to stdout as JSON Lines.
    pub json: bool,
    /// Per-file lines on stderr.
    pub progress: bool,
    /// No warnings.
    pub quiet: bool,
    /// `-v` count.
    pub verbose: u8,
    /// ANSI colour on stderr.
    pub color: bool,
}

/// The stderr and stdout writer shared by every worker.
#[derive(Debug)]
pub struct Printer {
    /// The flags.
    pub feedback: Feedback,
    lock: Mutex<()>,
}

const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

impl Printer {
    /// A printer with the given settings.
    pub fn new(feedback: Feedback) -> Self {
        Self {
            feedback,
            lock: Mutex::new(()),
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.feedback.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    /// An error line. Always printed.
    pub fn error(&self, message: &str) {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        eprintln!("{}: {message}", self.paint(RED, "error"));
    }

    /// One finished output: the JSON line if `--json`, the human line if
    /// progress is on, the error line if it failed. `details` are the
    /// `-vv` lines.
    pub fn record(&self, r: &Record, details: &[String]) {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fb = self.feedback;
        if fb.json {
            let mut out = std::io::stdout().lock();
            if let Ok(line) = serde_json::to_string(r) {
                let _ = writeln!(out, "{line}");
            }
            let _ = out.flush();
        }
        let mut err = std::io::stderr().lock();
        match r.status {
            Status::Failed => {
                let where_ = match &r.output {
                    Some(o) => format!("{} -> {o}", r.input),
                    None => r.input.clone(),
                };
                let _ = writeln!(
                    err,
                    "{}: {where_}: {}",
                    self.paint(RED, "error"),
                    r.error.as_deref().unwrap_or("failed")
                );
            }
            _ if fb.progress => {
                let _ = writeln!(err, "{}", Self::human_line(r));
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
                self.paint(DIM, &format!("trials: {}", list.join(", ")))
            );
        }
        if fb.verbose >= 2 {
            for d in details {
                let _ = writeln!(err, "  {}", self.paint(DIM, d));
            }
        }
        if let (Some(false), Some(score), Some(target)) = (r.reached, r.score, r.target)
            && !fb.quiet
            && r.status == Status::Written
        {
            let q = r.quality.map_or(String::new(), |q| format!(" at q{q}"));
            let _ = writeln!(
                err,
                "{}: {} -> {}: target {target} not reached, best score {score:.1}{q}",
                self.paint(YELLOW, "warning"),
                r.input,
                r.output.as_deref().unwrap_or("-")
            );
        }
    }

    fn human_line(r: &Record) -> String {
        let arrow = |o: &Option<String>| match o {
            Some(o) => format!("{} -> {o}", r.input),
            None => r.input.clone(),
        };
        match r.status {
            Status::Written => {
                let sizes = match (r.input_bytes, r.output_bytes) {
                    (Some(i), Some(o)) => format!(
                        "  {} -> {} ({})",
                        fmt_bytes(i),
                        fmt_bytes(o),
                        fmt_change(i, o)
                    ),
                    (_, Some(o)) => format!("  {}", fmt_bytes(o)),
                    _ => String::new(),
                };
                let how = match (r.lossless, r.quality, r.score) {
                    (Some(true), _, _) => " lossless".to_string(),
                    (_, Some(q), Some(s)) => format!(" q{q} s{s:.1}"),
                    (_, Some(q), None) => format!(" q{q}"),
                    _ => String::new(),
                };
                format!(
                    "{}{sizes}  {}{how}",
                    arrow(&r.output),
                    r.format.unwrap_or("")
                )
            }
            Status::Skipped => format!(
                "{}  skipped: {}",
                arrow(&r.output),
                r.reason.as_deref().unwrap_or("")
            ),
            Status::Planned => {
                let dims = match (r.width, r.height) {
                    (Some(w), Some(h)) => format!("{w}x{h}"),
                    _ => String::new(),
                };
                let alpha = if r.alpha == Some(true) { " alpha" } else { "" };
                format!(
                    "{}  {dims}{alpha} {} {} -> {} ({})",
                    r.input,
                    r.content.unwrap_or(""),
                    r.input_format.unwrap_or("?"),
                    r.output.as_deref().unwrap_or("-"),
                    r.format.unwrap_or("?")
                )
            }
            Status::Failed => unreachable!("failures are rendered separately"),
        }
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
    let native: Vec<&str> = format
        .encoder_features()
        .iter()
        .copied()
        .filter(|f| existing.is_none() || f.starts_with("native-"))
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
