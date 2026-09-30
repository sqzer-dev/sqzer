//! One input through the pipeline: read, probe, reserve its memory,
//! decode and prepare once, resize once per width, then encode and place
//! every requested output.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sqzer::core::Decoded;
use sqzer::core::codec::Format;
use sqzer::core::params::{Resize, Resolved, Target};
use sqzer::core::resize::{Geometry, Size};
use sqzer::{Ready, Sqzer};

use crate::budget::{Cost, MemoryBudget, Work, output_pixels};
use crate::cli::format_name;
use crate::config::Config;
use crate::inputs::Input;
use crate::output::{Naming, stem_of};
use crate::report::{
    Printer, Record, Stage, Status, Tally, Worker, content_name, describe_error, fmt_bytes,
};

/// What every job shares.
pub struct Ctx<'a> {
    /// The run.
    pub cfg: &'a Config,
    /// The memory budget.
    pub budget: &'a MemoryBudget,
    /// Where records go.
    pub printer: &'a Printer,
}

/// Process one input, printing each output's record as it completes.
/// Returns the counts for the summary.
pub fn process(input: &Input, ctx: &Ctx<'_>) -> Tally {
    let mut tally = Tally::default();
    let cfg = ctx.cfg;
    let name = input.display();
    let worker = ctx.printer.worker(&name);
    let mut fail = |rec: Record| {
        tally.add(&rec);
        ctx.printer.record(&rec, &[]);
        tally
    };
    let bytes = match read(input) {
        Ok(b) => b,
        Err(e) => return fail(Record::failed(name, &format!("cannot read: {e}"))),
    };
    let input_len = bytes.len() as u64;
    let sizes = sizes(cfg);
    // The header sizes the budget reservation; a file no usable decoder
    // claims reserves the maximum and gets its error from the decode
    // below, which knows whether the format is unknown or merely
    // unreadable in this build.
    let dimensions = cfg
        .sqzer
        .registry()
        .probe(&bytes)
        .and_then(|(_, decoder)| decoder.dimensions(&bytes));
    let cost = dimensions.map_or_else(
        || Cost {
            bytes: cfg.work.estimate(cfg.max_pixels, cfg.max_pixels),
            pixels: cfg.max_pixels,
        },
        |(w, h)| {
            let pixels = u64::from(w) * u64::from(h);
            let out = output_pixels(&sizes, w, h);
            Cost {
                bytes: cfg.work.estimate(pixels, out),
                pixels,
            }
        },
    );
    let estimate = cost.bytes;
    if let Some((w, h)) = dimensions
        && estimate > ctx.budget.limit().bytes
    {
        ctx.printer
            .warning(&oversized(&name, (w, h), estimate, ctx));
    }
    let _reservation = ctx.budget.reserve(cost);

    if let Some(w) = &worker {
        w.stage(Stage::Decode);
    }
    let decoded = match cfg.sqzer.decode(&bytes) {
        Ok(d) => d,
        Err(e) => return fail(Record::failed(name, &describe_error(&e))),
    };
    // The record describes the input; everything after this line sees the
    // image the encoder will get.
    let emit = Emit {
        input,
        base: describe(input, &decoded, input_len),
        ctx,
        worker: worker.as_ref(),
    };
    emit.sizes(decoded, &sizes, &mut tally);
    tally
}

/// Everything after the decode, for one input: the resize of each size
/// and the outputs of each.
struct Emit<'a> {
    input: &'a Input,
    /// The record fields known from the input.
    base: Record,
    ctx: &'a Ctx<'a>,
    worker: Option<&'a Worker<'a>>,
}

impl Emit<'_> {
    /// Prepare `decoded`, resize it to each of `sizes` and write every
    /// format of each.
    fn sizes(&self, decoded: Decoded, sizes: &[Resize], tally: &mut Tally) {
        let cfg = self.ctx.cfg;
        let (width, height) = (decoded.image.width(), decoded.image.height());

        // One size: the prepared image is resized in place of a copy.
        if cfg.widths.is_empty() {
            let geometry = cfg.sqzer.resize_bounds().fit(width, height);
            self.stage_resize(geometry);
            match cfg.sqzer.transform(decoded) {
                Ok(ready) => self.formats(&ready, geometry, tally),
                Err(e) => self.failed(&e, tally),
            }
            return;
        }

        // A width list: every width from the one prepared image. Widths
        // that come out the same size would write the same file.
        let geometries: Vec<Option<Geometry>> =
            sizes.iter().map(|r| r.fit(width, height)).collect();
        let dims: Vec<(u32, u32)> = geometries
            .iter()
            .map(|g| g.map_or((width, height), |g| g.output()))
            .collect();
        if let Some((i, j)) = (0..dims.len())
            .flat_map(|j| (0..j).map(move |i| (i, j)))
            .find(|&(i, j)| dims[i] == dims[j])
        {
            let (w, h) = dims[i];
            let hint = if cfg.sqzer.resize_bounds().enlarge {
                "drop one"
            } else {
                "`--enlarge` scales it up, or drop one"
            };
            let msg = format!(
                "widths {} and {} both give {w}x{h} from this {width}x{height} image; {hint}",
                cfg.widths[i], cfg.widths[j]
            );
            return self.failed(&msg, tally);
        }
        let prepared = match cfg.sqzer.prepare(decoded) {
            Ok(d) => d,
            Err(e) => return self.failed(&e, tally),
        };
        for (resize, geometry) in sizes.iter().zip(geometries) {
            self.stage_resize(geometry);
            match prepared.resize(resize) {
                Ok(ready) => self.formats(&ready, geometry, tally),
                Err(e) => self.failed(&e, tally),
            }
        }
    }

    /// Plan or write every requested format of one size.
    fn formats(&self, ready: &Ready, geometry: Option<Geometry>, tally: &mut Tally) {
        let cfg = self.ctx.cfg;
        let formats: Vec<Option<Format>> = if cfg.formats.is_empty() {
            vec![None]
        } else {
            cfg.formats.iter().copied().map(Some).collect()
        };
        for format in formats {
            let sqzer = match format {
                Some(f) => cfg.sqzer.clone().format(f),
                None => cfg.sqzer.clone(),
            };
            let base = self.base.clone();
            let (record, details) = if cfg.dry_run {
                let rec = plan(self.input, ready, &sqzer, base, geometry, self.ctx);
                (rec, Vec::new())
            } else {
                run(self.input, ready, &sqzer, base, self.ctx, self.worker)
            };
            tally.add(&record);
            self.ctx.printer.record(&record, &details);
        }
    }

    fn stage_resize(&self, geometry: Option<Geometry>) {
        if let Some(w) = self.worker
            && geometry.is_some()
        {
            w.stage(Stage::Resize);
        }
    }

    fn failed(&self, error: &dyn std::fmt::Display, tally: &mut Tally) {
        let rec = self.base.clone().fail(error);
        tally.add(&rec);
        self.ctx.printer.record(&rec, &[]);
    }
}

/// The resize of each output size: the one on `sqzer`, or one per width
/// of a `--width` list, each with the list's shared height.
fn sizes(cfg: &Config) -> Vec<Resize> {
    let base = cfg.sqzer.resize_bounds();
    if cfg.widths.is_empty() {
        return vec![base];
    }
    let height = match base.size {
        Size::Box { height, .. } => height,
        Size::Scale(_) => None,
    };
    cfg.widths
        .iter()
        .map(|&w| Resize {
            size: Size::Box {
                width: Some(w),
                height,
            },
            ..base
        })
        .collect()
}

/// What a resize crops or pads, for the dry run.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn resize_note(g: &Geometry) -> Option<String> {
    let px = |v: f64| v.round() as u64;
    if let Some(c) = g.crop {
        return Some(format!(
            "crop {}x{} at {},{}",
            px(c.width),
            px(c.height),
            px(c.left),
            px(c.top)
        ));
    }
    g.canvas
        .map(|c| format!("pad {}x{} at {},{}", g.width, g.height, c.x, c.y))
}

/// The warning for a file whose estimate alone is over the budget. It
/// still runs, alone, since refusing a file that may well fit is worse.
fn oversized(name: &str, (w, h): (u32, u32), estimate: u64, ctx: &Ctx<'_>) -> String {
    let limit = fmt_bytes(ctx.budget.limit().bytes);
    let need = fmt_bytes(estimate);
    let mut msg = format!(
        "{name}: {w}x{h} needs about {need}, more than the {limit} this run may use; it runs alone"
    );
    if ctx.cfg.work == Work::Search {
        msg.push_str(
            "\n  the target search is most of that; `-q` sets an explicit quality and skips it",
        );
    }
    msg
}

fn read(input: &Input) -> std::io::Result<Vec<u8>> {
    match input {
        Input::Stdin => {
            let mut buf = Vec::new();
            std::io::stdin().lock().read_to_end(&mut buf)?;
            Ok(buf)
        }
        Input::File(p) | Input::InTree { path: p, .. } => fs::read(p),
    }
}

/// The record fields that are known once the input is decoded.
fn describe(input: &Input, decoded: &Decoded, input_len: u64) -> Record {
    let img = &decoded.image;
    Record {
        input: input.display(),
        input_format: Some(format_name(decoded.info.format)),
        animated: Some(decoded.info.animated),
        width: Some(img.width()),
        height: Some(img.height()),
        alpha: Some(img.has_alpha()),
        input_bytes: Some(input_len),
        ..Record::default()
    }
}

/// `--dry-run`: resolve the format and the path, encode nothing.
fn plan(
    input: &Input,
    ready: &Ready,
    sqzer: &Sqzer,
    mut rec: Record,
    geometry: Option<Geometry>,
    ctx: &Ctx<'_>,
) -> Record {
    let format = sqzer.pick_format(ready);
    rec.format = Some(format_name(format));
    rec.content = Some(content_name(sqzer::core::content::classify(ready.image())));
    let (width, height) = ready.output_size();
    rec.output_width = Some(width);
    rec.output_height = Some(height);
    rec.resize_note = geometry.as_ref().and_then(resize_note);
    let caps = match sqzer.registry().encoder(format) {
        Ok(e) => e.caps(),
        Err(e) => return rec.fail(&e),
    };
    rec.backend = Some(caps.name);
    rec.tier = Some(caps.tier.to_string());
    // What the encoder would refuse, refused here too, so a plan is one
    // the run can carry out.
    if ready.translucent_padding() && !caps.alpha {
        return rec.fail(&format!(
            "{} has no alpha channel for a translucent `--background`",
            caps.name
        ));
    }
    let img = ready.image();
    for (blob, can) in [("EXIF", caps.exif), ("XMP", caps.xmp)] {
        let carried = if blob == "EXIF" {
            img.exif().is_some()
        } else {
            img.xmp().is_some()
        };
        if carried && !can {
            return rec.fail(&format!(
                "{} does not support {blob} metadata; drop `--keep-metadata` or pick another format",
                caps.name
            ));
        }
    }
    let quality = match sqzer.params().target {
        Target::Quality(q) => Some(Resolved::Quality(q)),
        Target::Lossless => Some(Resolved::Lossless),
        Target::Ssimulacra2(_) if !caps.lossy => Some(Resolved::Lossless),
        Target::Ssimulacra2(t) => {
            rec.target = Some(t);
            None
        }
    };
    match destination(
        input,
        ready.info().format,
        (width, height),
        format,
        quality,
        ctx,
    ) {
        Ok(Destination::Stdout) => rec.output = Some("-".into()),
        Ok(Destination::File(path)) => {
            rec.output = Some(path.display().to_string());
            if path.exists() && !ctx.cfg.force && !ctx.cfg.placement.in_place {
                return rec.fail(&"output exists; `--force` overwrites it");
            }
        }
        Err(e) => return rec.fail(&e),
    }
    rec.status = Status::Planned;
    rec
}

/// Encode, place, write.
fn run(
    input: &Input,
    ready: &Ready,
    sqzer: &Sqzer,
    rec: Record,
    ctx: &Ctx<'_>,
    worker: Option<&Worker<'_>>,
) -> (Record, Vec<String>) {
    let cfg = ctx.cfg;
    let stage = |s: Stage| {
        if let Some(w) = worker {
            w.stage(s);
        }
    };
    stage(Stage::Encode);
    let out = match sqzer.encode_with(ready, |p| stage(Stage::from(p))) {
        Ok(o) => o,
        Err(e) => return (rec.fail(&e), Vec::new()),
    };
    let mut rec = rec.with_output(&out);
    if let Target::Ssimulacra2(t) = sqzer.params().target {
        rec.target = Some(t);
    }
    let details = vec![format!(
        "params: backend {} ({}), effort {}, subsampling {:?}, keep_icc {}, keep_metadata {}, opts {}",
        out.backend,
        out.tier,
        sqzer.params().effort,
        sqzer.params().subsampling,
        sqzer.params().keep_icc,
        sqzer.params().keep_metadata,
        if sqzer.params().codec_specific.is_empty() {
            "none".to_string()
        } else {
            sqzer
                .params()
                .codec_specific
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        }
    )];

    let dest = match destination(
        input,
        out.input.format,
        (out.width, out.height),
        out.format,
        Some(out.target),
        ctx,
    ) {
        Ok(d) => d,
        Err(e) => return (rec.fail(&e), details),
    };
    let path = match dest {
        Destination::Stdout => {
            rec.output = Some("-".into());
            let mut stdout = std::io::stdout().lock();
            return match stdout.write_all(&out.bytes).and_then(|()| stdout.flush()) {
                Ok(()) => {
                    rec.status = Status::Written;
                    (rec, details)
                }
                Err(e) => (rec.fail(&format!("cannot write to stdout: {e}")), details),
            };
        }
        Destination::File(p) => p,
    };
    rec.output = Some(path.display().to_string());

    let input_len = rec.input_bytes.unwrap_or(0);
    if !cfg.force && out.bytes.len() as u64 > input_len {
        rec.status = Status::Skipped;
        rec.reason = Some(format!(
            "output ({} bytes) is larger than input ({input_len} bytes); `--force` writes it anyway",
            out.bytes.len()
        ));
        return (rec, details);
    }
    if cfg.placement.in_place {
        if cfg.backup {
            let backup = backup_path(&path);
            if backup.exists() && !cfg.force {
                return (
                    rec.fail(&format!(
                        "backup {} exists; `--force` overwrites it",
                        backup.display()
                    )),
                    details,
                );
            }
            if let Err(e) = fs::rename(&path, &backup) {
                return (
                    rec.fail(&format!("cannot move input to {}: {e}", backup.display())),
                    details,
                );
            }
        }
    } else if path.exists() && !cfg.force {
        return (rec.fail(&"output exists; `--force` overwrites it"), details);
    }
    stage(Stage::Write);
    match write_atomic(&path, &out.bytes) {
        Ok(()) => rec.status = Status::Written,
        Err(e) => return (rec.fail(&format!("cannot write: {e}")), details),
    }
    (rec, details)
}

enum Destination {
    Stdout,
    File(PathBuf),
}

fn destination(
    input: &Input,
    input_format: Format,
    (width, height): (u32, u32),
    format: Format,
    quality: Option<Resolved>,
    ctx: &Ctx<'_>,
) -> Result<Destination, String> {
    let placement = &ctx.cfg.placement;
    if *input == Input::Stdin {
        return Ok(match &placement.output {
            None => Destination::Stdout,
            Some(p) if placement.single_file => Destination::File(p.clone()),
            Some(dir) => Destination::File(dir.join(format!(
                "stdin{}.{}",
                placement.width_tag(width),
                format.extension()
            ))),
        });
    }
    let naming = Naming {
        input,
        input_format,
        format,
        width,
        height,
        quality,
    };
    placement
        .resolve(&naming)
        .map(Destination::File)
        .map_err(|e| e.to_string())
}

/// `photo@backup.jpg` next to `photo.jpg`.
fn backup_path(input: &Path) -> PathBuf {
    let stem = stem_of(input);
    let name = match input.extension() {
        Some(ext) => format!("{stem}@backup.{}", ext.to_string_lossy()),
        None => format!("{stem}@backup"),
    };
    input.with_file_name(name)
}

/// Write to a sibling temporary file, then rename over `path`, so a
/// crash mid-write never leaves a truncated output.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{file_name}.{}.sqzer-tmp", std::process::id()));
    let result = fs::write(&tmp, bytes).and_then(|()| match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        // Windows will not rename over an existing file.
        Err(_) if path.exists() => {
            fs::remove_file(path)?;
            fs::rename(&tmp, path)
        }
        Err(e) => Err(e),
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_keep_the_extension() {
        assert_eq!(
            backup_path(Path::new("dir/photo.jpg")),
            PathBuf::from("dir/photo@backup.jpg")
        );
        assert_eq!(
            backup_path(Path::new("a.b.png")),
            PathBuf::from("a.b@backup.png")
        );
        assert_eq!(
            backup_path(Path::new("noext")),
            PathBuf::from("noext@backup")
        );
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("sqzer-job-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let target = dir.join("deep/er/out.bin");
        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("sqzer-tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
