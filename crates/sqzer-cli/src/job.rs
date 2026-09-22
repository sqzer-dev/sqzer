//! One input through the pipeline: read, probe, reserve its pixels,
//! decode and resize once, then encode and place every requested output.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sqzer::Sqzer;
use sqzer::core::Decoded;
use sqzer::core::codec::Format;
use sqzer::core::params::{Resolved, Target};

use crate::budget::PixelBudget;
use crate::cli::format_name;
use crate::config::Config;
use crate::inputs::Input;
use crate::output::{Naming, stem_of};
use crate::report::{Printer, Record, Stage, Status, Tally, Worker, content_name};

/// What every job shares.
pub struct Ctx<'a> {
    /// The run.
    pub cfg: &'a Config,
    /// The decoded-pixel budget.
    pub budget: &'a PixelBudget,
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
    // The header sizes the budget reservation; a file no usable decoder
    // claims reserves the maximum and gets its error from the decode
    // below, which knows whether the format is unknown or merely
    // unreadable in this build.
    let pixels = cfg
        .sqzer
        .registry()
        .probe(&bytes)
        .and_then(|(_, decoder)| decoder.dimensions(&bytes))
        .map_or(cfg.max_pixels, |(w, h)| u64::from(w) * u64::from(h));
    let _reservation = ctx.budget.reserve(pixels);

    if let Some(w) = &worker {
        w.stage(Stage::Decode);
    }
    let decoded = match cfg.sqzer.decode(&bytes) {
        Ok(d) => d,
        Err(e) => return fail(Record::failed(name, &e)),
    };
    // The record describes the input; everything after this line sees the
    // image the encoder will get.
    let base = describe(input, &decoded, input_len);
    let (width, height) = (decoded.image.width(), decoded.image.height());
    if let Some(w) = &worker
        && cfg.sqzer.resize_bounds().fit(width, height).is_some()
    {
        w.stage(Stage::Resize);
    }
    let decoded = match cfg.sqzer.transform(decoded) {
        Ok(d) => d,
        Err(e) => return fail(base.fail(&e)),
    };

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
        let base = base.clone();
        let (record, details) = if cfg.dry_run {
            (plan(input, &decoded, &sqzer, base, ctx), Vec::new())
        } else {
            run(input, &decoded, &sqzer, base, ctx, worker.as_ref())
        };
        tally.add(&record);
        ctx.printer.record(&record, &details);
    }
    tally
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
fn plan(input: &Input, decoded: &Decoded, sqzer: &Sqzer, mut rec: Record, ctx: &Ctx<'_>) -> Record {
    let format = sqzer.pick_format(decoded);
    rec.format = Some(format_name(format));
    rec.content = Some(content_name(sqzer::core::content::classify(&decoded.image)));
    rec.output_width = Some(decoded.image.width());
    rec.output_height = Some(decoded.image.height());
    let caps = match sqzer.registry().encoder(format) {
        Ok(e) => e.caps(),
        Err(e) => return rec.fail(&e),
    };
    rec.backend = Some(caps.name);
    rec.tier = Some(caps.tier.to_string());
    let quality = match sqzer.params().target {
        Target::Quality(q) => Some(Resolved::Quality(q)),
        Target::Lossless => Some(Resolved::Lossless),
        Target::Ssimulacra2(_) if !caps.lossy => Some(Resolved::Lossless),
        Target::Ssimulacra2(t) => {
            rec.target = Some(t);
            None
        }
    };
    match destination(input, decoded, format, quality, ctx) {
        Ok(Destination::Stdout) => rec.output = Some("-".into()),
        Ok(Destination::File(path)) => {
            rec.output = Some(path.display().to_string());
            if path.exists() && !ctx.cfg.force && !ctx.cfg.placement.in_place {
                return rec.fail(&"output exists; --force overwrites it");
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
    decoded: &Decoded,
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
    let out = match sqzer.encode_with(decoded, |p| stage(Stage::from(p))) {
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

    let dest = match destination(input, decoded, out.format, Some(out.target), ctx) {
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
            "output ({} bytes) is larger than input ({input_len} bytes); --force writes it anyway",
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
                        "backup {} exists; --force overwrites it",
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
        return (rec.fail(&"output exists; --force overwrites it"), details);
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
    decoded: &Decoded,
    format: Format,
    quality: Option<Resolved>,
    ctx: &Ctx<'_>,
) -> Result<Destination, String> {
    let placement = &ctx.cfg.placement;
    if *input == Input::Stdin {
        return Ok(match &placement.output {
            None => Destination::Stdout,
            Some(p) if placement.single_file => Destination::File(p.clone()),
            Some(dir) => Destination::File(dir.join(format!("stdin.{}", format.extension()))),
        });
    }
    let naming = Naming {
        input,
        input_format: decoded.info.format,
        format,
        width: decoded.image.width(),
        height: decoded.image.height(),
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
