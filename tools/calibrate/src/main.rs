//! Calibration harness for the seed tables in `sqzer-metrics`.
//!
//! `sweep` encodes every image of a corpus at a grid of qualities on every
//! lossy backend, scores each result with SSIMULACRA2 through `codec-eval`,
//! and writes, per backend and target score, the median quality at which
//! the corpus first reaches the target. `verify` runs `sqzer`'s own search
//! on a held-out split, seeded from the tables compiled into the library
//! and unseeded, and reports how many encodes each needed.
//!
//! Everything is decoded through `sqzer`'s registry and encoded through its
//! `Encoder` trait, so a table measures the backend as the pipeline runs
//! it: default effort, automatic chroma subsampling, sources treated as
//! sRGB with any ICC profile dropped.

// Counts and byte sizes averaged for a report: the precision is not needed.
#![allow(clippy::cast_precision_loss)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use clap::{Args, Parser, Subcommand};
use codec_eval::eval::session::EncodeRequest;
use codec_eval::{Corpus, EvalConfig, EvalSession, ImageData, MetricConfig};
use rayon::prelude::*;
use sqzer::core::codec::{Encoder, EncoderCaps, Format, Tier};
use sqzer::core::image::{ColorType, Image};
use sqzer::core::params::{DecodeOpts, EncodeParams, Resolved, Target};
use sqzer::core::{Error as SqzerError, Registry, Result as SqzerResult};
use sqzer::metrics::{Reference, Search, seeds};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Targets a table has a point for.
const TARGETS: &[f64] = &[
    20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 55.0, 60.0, 65.0, 70.0, 75.0, 80.0, 85.0, 90.0, 95.0,
];

#[derive(Parser)]
#[command(name = "sqzer-calibrate", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sweep every lossy backend over the corpus and write the seed tables.
    Sweep(SweepArgs),
    /// Run the target search seeded and unseeded on a held-out split.
    Verify(VerifyArgs),
}

#[derive(Args)]
struct Common {
    /// codec-corpus dataset paths, comma separated. Downloaded on first
    /// use and cached under the user's cache directory.
    #[arg(long, value_delimiter = ',')]
    datasets: Vec<String>,
    /// Formats to run. Default: every lossy encoder this build can also
    /// decode.
    #[arg(long, value_delimiter = ',')]
    formats: Vec<String>,
    /// Images to take from each dataset, in path order. 0 means all.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Worker threads. Default: every core. Each worker holds one decoded
    /// source and one encode in flight, so lower this on a small machine
    /// when the corpus has large images.
    #[arg(long)]
    jobs: Option<usize>,
    /// Where per-image JSON reports go. Default: `target/calibration`
    /// under the repository.
    #[arg(long)]
    report_dir: Option<PathBuf>,
}

#[derive(Args)]
struct SweepArgs {
    #[command(flatten)]
    common: Common,
    /// File to write. Default: the tables module of `sqzer-metrics`.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct VerifyArgs {
    #[command(flatten)]
    common: Common,
    /// Targets to search for, comma separated.
    #[arg(long, value_delimiter = ',', default_value = "50,70,85")]
    targets: Vec<f32>,
}

const SWEEP_DATASETS: &[&str] = &["CID22/CID22-512/training", "gb82-sc", "clic2025/training"];
const VERIFY_DATASETS: &[&str] = &["CID22/CID22-512/validation"];

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Sweep(args) => sweep(args),
        Cmd::Verify(args) => verify(&args),
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// What both commands start from.
struct Setup {
    registry: Arc<Registry>,
    backends: Vec<Backend>,
    sources: Vec<Source>,
    report_dir: PathBuf,
}

fn setup(common: &Common, default_datasets: &[&str]) -> Result<Setup> {
    if let Some(jobs) = common.jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global()?;
    }
    let registry = Arc::new(registry());
    let backends = backends(&registry, &common.formats)?;
    if backends.is_empty() {
        return Err("no lossy backend to calibrate".into());
    }
    let datasets: Vec<String> = if common.datasets.is_empty() {
        default_datasets.iter().map(ToString::to_string).collect()
    } else {
        common.datasets.clone()
    };
    let sources = load_sources(&registry, &datasets, common.limit)?;
    if sources.is_empty() {
        return Err("no PNG sources in the chosen datasets".into());
    }
    let report_dir = common
        .report_dir
        .clone()
        .unwrap_or_else(|| repo_root().join("target/calibration"));
    Ok(Setup {
        registry,
        backends,
        sources,
        report_dir,
    })
}

// ---------------------------------------------------------------------------
// Backends
// ---------------------------------------------------------------------------

/// One encoder under calibration.
struct Backend {
    format: Format,
    tier: Tier,
    /// Directory and codec id, e.g. `avif-portable`.
    id: String,
    /// Crate and version, for the table header.
    label: String,
    /// Qualities to sweep.
    grid: Vec<f64>,
}

/// `sqzer`'s registry plus the tool-only lossy WebP encoder when the build
/// has none of its own.
fn registry() -> Registry {
    let mut reg = sqzer::codecs::registry();
    let has_lossy_webp = reg
        .encoders()
        .any(|e| e.caps().format == Format::WebP && e.caps().lossy);
    if !has_lossy_webp {
        reg.register_encoder(LibwebpEncoder);
    }
    reg
}

fn backends(registry: &Registry, wanted: &[String]) -> Result<Vec<Backend>> {
    let wanted: Vec<Format> = wanted
        .iter()
        .map(|s| Format::from_extension(s).ok_or_else(|| format!("unknown format {s}")))
        .collect::<std::result::Result<_, _>>()?;
    let lock = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"))?;
    let mut out = Vec::new();
    for format in Format::ALL {
        let Ok(enc) = registry.encoder(*format) else {
            continue;
        };
        let caps = enc.caps();
        if !caps.lossy || !registry.has_decoder(*format) {
            continue;
        }
        if !wanted.is_empty() && !wanted.contains(format) {
            continue;
        }
        out.push(Backend {
            format: *format,
            tier: caps.tier,
            id: format!("{}-{}", format.extension(), caps.tier),
            label: label(caps, &lock),
            grid: grid(*format),
        });
    }
    Ok(out)
}

/// Crate and version behind a backend, read from this tool's lock file so
/// the table header names what actually ran.
fn label(caps: &EncoderCaps, lock: &str) -> String {
    let crates: &[&str] = match (caps.format, caps.tier) {
        (Format::Jpeg, Tier::Portable) => &["mozjpeg-rs"],
        (Format::Avif, Tier::Portable) => &["ravif", "rav1e"],
        (Format::WebP, Tier::Native) => &["libwebp-sys"],
        _ => &[],
    };
    if crates.is_empty() {
        return format!("{} {} tier", caps.format, caps.tier);
    }
    crates
        .iter()
        .map(|name| format!("{name} {}", lock_version(lock, name)))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn lock_version(lock: &str, name: &str) -> String {
    let needle = format!("name = \"{name}\"");
    lock.lines()
        .skip_while(|l| l.trim() != needle)
        .nth(1)
        .and_then(|l| l.trim().strip_prefix("version = \""))
        .and_then(|l| l.strip_suffix('"'))
        .unwrap_or("?")
        .to_string()
}

/// Quality grid per format. AV1 encodes are two orders of magnitude slower
/// than JPEG, so they get a coarser grid; the crossing is interpolated
/// between neighbours either way.
fn grid(format: Format) -> Vec<f64> {
    let step = match format {
        Format::Avif => 5,
        _ => 2,
    };
    (1..=100 / step).map(|i| f64::from(i * step)).collect()
}

// ---------------------------------------------------------------------------
// Tool-only lossy WebP
// ---------------------------------------------------------------------------

/// `libwebp` through the `webp` crate, standing in for the `native-webp`
/// backend of ADR-0001 item 8. Abstract quality maps one to one onto
/// `libwebp`'s, at its default method (4). Re-run the sweep through the
/// real backend once it exists; this type then goes.
struct LibwebpEncoder;

static LIBWEBP_CAPS: EncoderCaps = EncoderCaps {
    format: Format::WebP,
    lossy: true,
    lossless: true,
    alpha: false,
    animation: false,
    bit_depth: &[8],
    hdr: false,
    quality_range: 0.0..=100.0,
    effort_range: 4..=4,
    tier: Tier::Native,
};

impl Encoder for LibwebpEncoder {
    fn caps(&self) -> &EncoderCaps {
        &LIBWEBP_CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> SqzerResult<Vec<u8>> {
        let img = rgb8(img)?;
        let samples = img
            .samples()
            .as_u8()
            .ok_or_else(|| SqzerError::Codec("expected 8-bit samples".into()))?;
        let enc = webp::Encoder::from_rgb(samples, img.width(), img.height());
        let mem = match params.resolved()? {
            Resolved::Lossless => enc.encode_lossless(),
            Resolved::Quality(q) => enc.encode(q),
        };
        Ok(mem.to_vec())
    }
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// One decoded source image, RGB8, no ICC.
struct Source {
    dataset: String,
    name: String,
    image: Image,
}

fn load_sources(registry: &Registry, datasets: &[String], limit: usize) -> Result<Vec<Source>> {
    let mut sources = Vec::new();
    for dataset in datasets {
        let corpus = Corpus::get_dataset(dataset)?;
        let mut images: Vec<_> = corpus
            .images
            .iter()
            .filter(|i| i.format.eq_ignore_ascii_case("png"))
            .collect();
        images.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        if limit > 0 {
            images.truncate(limit);
        }
        eprintln!("{dataset}: {} PNG sources", images.len());
        let decoded: Vec<Source> = images
            .par_iter()
            .map(|img| -> Result<Source> {
                let path = img.full_path(&corpus.root_path);
                let bytes = std::fs::read(&path)?;
                let image = registry.decode(&bytes, &DecodeOpts::default())?.image;
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(Source {
                    dataset: dataset.clone(),
                    name: format!("{}__{stem}", dataset.replace('/', "_")),
                    image: rgb8(&image)?,
                })
            })
            .collect::<Result<_>>()?;
        sources.extend(decoded);
    }
    Ok(sources)
}

/// `img` as RGB8 with no ICC: alpha dropped, gray widened, deeper samples
/// rounded. Every encoder sees the same bytes the metric scores.
fn rgb8(img: &Image) -> SqzerResult<Image> {
    let u8 = img.to_u8(Format::Png)?;
    let no_alpha = u8.without_alpha();
    let out = match no_alpha.color() {
        ColorType::Rgb => no_alpha.into_owned(),
        ColorType::Gray => {
            let samples = no_alpha
                .samples()
                .as_u8()
                .ok_or_else(|| SqzerError::Codec("expected 8-bit samples".into()))?;
            let rgb = samples.iter().flat_map(|&g| [g, g, g]).collect();
            Image::from_u8(no_alpha.width(), no_alpha.height(), ColorType::Rgb, rgb)?
        }
        other => {
            return Err(SqzerError::Codec(format!(
                "unexpected colour type after dropping alpha: {other:?}"
            )));
        }
    };
    Ok(out.with_icc(None))
}

fn image_data(img: &Image) -> ImageData {
    ImageData::RgbSlice {
        data: img.samples().as_u8().expect("rgb8 sources").to_vec(),
        width: img.width() as usize,
        height: img.height() as usize,
    }
}

fn image_from(data: &ImageData) -> SqzerResult<Image> {
    let (w, h) = (data.width(), data.height());
    Image::from_u8(
        u32::try_from(w).map_err(|e| SqzerError::Codec(e.to_string()))?,
        u32::try_from(h).map_err(|e| SqzerError::Codec(e.to_string()))?,
        ColorType::Rgb,
        data.to_rgb8_vec(),
    )
}

fn codec_error(codec: &str, e: &SqzerError) -> codec_eval::Error {
    codec_eval::Error::Codec {
        codec: codec.to_string(),
        message: e.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Sweep
// ---------------------------------------------------------------------------

/// Per image, the score at every grid quality.
type Curves = BTreeMap<String, Vec<(f64, f64)>>;

fn sweep(args: SweepArgs) -> Result<()> {
    let Setup {
        registry,
        backends,
        sources,
        report_dir,
    } = setup(&args.common, SWEEP_DATASETS)?;
    let out = args
        .out
        .unwrap_or_else(|| repo_root().join("crates/sqzer-metrics/src/seeds/tables.rs"));
    let corpus_label = corpus_label(&sources);
    eprintln!(
        "{} sources, {} backends, reports in {}",
        sources.len(),
        backends.len(),
        report_dir.display()
    );

    let mut tables = Vec::new();
    for backend in &backends {
        let started = Instant::now();
        let curves = sweep_backend(&registry, backend, &sources, &report_dir)?;
        eprintln!(
            "{}: {} images in {:.0?}",
            backend.id,
            curves.len(),
            started.elapsed()
        );
        tables.push(fit(backend, &corpus_label, &curves));
    }

    let code = render(&tables);
    std::fs::write(&out, code)?;
    eprintln!("wrote {}", out.display());
    for t in &tables {
        print_table(t);
    }
    Ok(())
}

fn sweep_backend(
    registry: &Arc<Registry>,
    backend: &Backend,
    sources: &[Source],
    report_dir: &Path,
) -> Result<Curves> {
    let dir = report_dir.join(&backend.id);
    std::fs::create_dir_all(&dir)?;
    let config = EvalConfig::builder()
        .report_dir(&dir)
        .metrics(MetricConfig {
            dssim: false,
            ssimulacra2: true,
            butteraugli: false,
            psnr: false,
            xyb_roundtrip: false,
        })
        .quality_levels(backend.grid.clone())
        .build();
    let mut session = EvalSession::new(config);
    let (enc_reg, dec_reg) = (Arc::clone(registry), Arc::clone(registry));
    let (format, id) = (backend.format, backend.id.clone());
    let enc_id = id.clone();
    session.add_codec_with_decode(
        &backend.id,
        &backend.label,
        Box::new(move |image: &ImageData, request: &EncodeRequest| {
            let image = image_from(image).map_err(|e| codec_error(&enc_id, &e))?;
            let params = EncodeParams {
                // Grid values are whole numbers; the cast is exact.
                #[allow(clippy::cast_possible_truncation)]
                target: Target::Quality(request.quality as f32),
                ..EncodeParams::default()
            };
            enc_reg
                .encoder(format)
                .and_then(|e| e.encode(&image, &params))
                .map_err(|e| codec_error(&enc_id, &e))
        }),
        Box::new(move |bytes: &[u8]| {
            let decoded = dec_reg
                .decode(bytes, &DecodeOpts::default())
                .and_then(|d| rgb8(&d.image))
                .map_err(|e| codec_error(&id, &e))?;
            Ok(image_data(&decoded))
        }),
    );

    let done = AtomicUsize::new(0);
    let total = sources.len();
    let results: Vec<(String, Vec<(f64, f64)>)> = sources
        .par_iter()
        .map(|src| -> Result<(String, Vec<(f64, f64)>)> {
            let started = Instant::now();
            let report = session.evaluate_image(&src.name, image_data(&src.image))?;
            session.write_image_report(&report)?;
            let mut curve: Vec<(f64, f64)> = report
                .results
                .iter()
                .map(|r| (r.quality, r.metrics.ssimulacra2.unwrap_or(f64::NAN)))
                .collect();
            curve.sort_by(|a, b| a.0.total_cmp(&b.0));
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!(
                "  [{n}/{total}] {} {}x{} {:.1?}",
                src.name,
                src.image.width(),
                src.image.height(),
                started.elapsed()
            );
            Ok((src.name.clone(), curve))
        })
        .collect::<Result<_>>()?;
    Ok(results.into_iter().collect())
}

/// Quality at which `curve` first reaches `target`, interpolated between
/// the grid points either side. `None` when it never does.
fn crossing(curve: &[(f64, f64)], target: f64) -> Option<f64> {
    let i = curve.iter().position(|&(_, s)| s >= target)?;
    if i == 0 {
        return Some(curve[0].0);
    }
    let (q0, s0) = curve[i - 1];
    let (q1, s1) = curve[i];
    if s1 <= s0 {
        return Some(q1);
    }
    Some(q0 + (q1 - q0) * (target - s0) / (s1 - s0))
}

/// A fitted table, ready to render.
struct Table {
    format: Format,
    tier: Tier,
    label: String,
    corpus: String,
    images: usize,
    points: Vec<Point>,
}

struct Point {
    target: f64,
    quality: f64,
    low: f64,
    high: f64,
    /// Images that never reached the target at the top of the grid.
    unreachable: usize,
}

fn fit(backend: &Backend, corpus: &str, curves: &Curves) -> Table {
    let points = TARGETS
        .iter()
        .map(|&target| {
            let hits: Vec<f64> = curves
                .values()
                .filter_map(|c| crossing(c, target))
                .collect();
            // A target no image reaches is seeded at the top of the grid,
            // where the search then finds the cap in one encode.
            let top = backend.grid.last().copied().unwrap_or(100.0);
            let (quality, low, high) = if hits.is_empty() {
                (top, top, top)
            } else {
                (
                    codec_eval::median(&hits),
                    codec_eval::percentile(&hits, 0.25),
                    codec_eval::percentile(&hits, 0.75),
                )
            };
            Point {
                target,
                quality,
                low,
                high,
                unreachable: curves.len() - hits.len(),
            }
        })
        .collect();
    Table {
        format: backend.format,
        tier: backend.tier,
        label: backend.label.clone(),
        corpus: corpus.to_string(),
        images: curves.len(),
        points,
    }
}

fn corpus_label(sources: &[Source]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for s in sources {
        *counts.entry(&s.dataset).or_default() += 1;
    }
    counts
        .iter()
        .map(|(d, n)| format!("{d} ({n})"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render(tables: &[Table]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "// Generated by `tools/calibrate sweep` on {}. Do not edit by hand;\n\
         // `tools/calibrate/README.md` says how to regenerate.\n\
         //\n\
         // Each point: at `target`, the median corpus image first reaches the\n\
         // score at `quality`; `low` and `high` are the quartiles. Calibrated at\n\
         // the default effort with automatic chroma subsampling, sources as\n\
         // sRGB with any ICC profile dropped.\n",
        today()
    );
    s.push_str("use sqzer_core::codec::{Format, Tier};\n\nuse super::{SeedPoint, SeedTable};\n\n");
    // One point per line reads as a table; rustfmt would spread each over
    // six lines.
    s.push_str("#[rustfmt::skip]\npub(super) static TABLES: &[SeedTable] = &[\n");
    for t in tables {
        let _ = writeln!(s, "    // {}: {}", t.label, t.corpus);
        let never: Vec<String> = t
            .points
            .iter()
            .filter(|p| p.unreachable > 0)
            .map(|p| format!("{} of {} at {}", p.unreachable, t.images, p.target))
            .collect();
        if !never.is_empty() {
            let _ = writeln!(
                s,
                "    // Images that never reach the target at quality 100: {}.",
                never.join(", ")
            );
        }
        let _ = writeln!(
            s,
            "    SeedTable {{\n        format: Format::{:?},\n        tier: Tier::{:?},\n        backend: {:?},\n        corpus: {:?},\n        images: {},\n        points: &[",
            t.format, t.tier, t.label, t.corpus, t.images
        );
        for p in &t.points {
            let _ = writeln!(
                s,
                "            SeedPoint {{ target: {:.1}, quality: {:.1}, low: {:.1}, high: {:.1} }},",
                p.target, p.quality, p.low, p.high
            );
        }
        s.push_str("        ],\n    },\n");
    }
    s.push_str("];\n");
    s
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn print_table(t: &Table) {
    println!(
        "{} ({}) on {}: {} images",
        t.format, t.tier, t.label, t.images
    );
    println!("  target  quality   q25   q75  unreachable");
    for p in &t.points {
        println!(
            "  {:>6.0}  {:>7.1}  {:>4.0}  {:>4.0}  {}",
            p.target, p.quality, p.low, p.high, p.unreachable
        );
    }
}

// ---------------------------------------------------------------------------
// Verify
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Tally {
    runs: usize,
    encodes: usize,
    first_hit: usize,
    reached: usize,
    quality: f64,
    bytes: f64,
}

impl Tally {
    fn add(&mut self, report: &sqzer::metrics::SearchReport, bytes: usize) {
        self.runs += 1;
        self.encodes += usize::from(report.iterations);
        self.first_hit += usize::from(report.iterations == 1 && report.reached);
        self.reached += usize::from(report.reached);
        self.quality += f64::from(report.quality);
        self.bytes += bytes as f64;
    }

    fn merge(mut self, other: Self) -> Self {
        self.runs += other.runs;
        self.encodes += other.encodes;
        self.first_hit += other.first_hit;
        self.reached += other.reached;
        self.quality += other.quality;
        self.bytes += other.bytes;
        self
    }
}

fn verify(args: &VerifyArgs) -> Result<()> {
    let Setup {
        registry,
        backends,
        sources,
        ..
    } = setup(&args.common, VERIFY_DATASETS)?;
    eprintln!("{} sources, {} backends", sources.len(), backends.len());
    println!("seeded vs unseeded search, {} images", sources.len());
    println!("  backend         target  mode      encodes  first-hit  reached  quality    bytes");
    for backend in &backends {
        let encoder = registry.encoder(backend.format)?;
        let table = seeds::table(backend.format, backend.tier);
        if table.is_none() {
            eprintln!("{}: no seed table compiled in, skipping", backend.id);
            continue;
        }
        for &target in &args.targets {
            let started = Instant::now();
            let (plain, seeded) = sources
                .par_iter()
                .map(|src| -> Result<(Tally, Tally)> {
                    let params = EncodeParams::default();
                    let mut reference = Reference::new(&src.image)?;
                    let mut plain = Tally::default();
                    let mut seeded = Tally::default();
                    let found = Search::new(target).encode(
                        encoder,
                        &src.image,
                        &params,
                        &registry,
                        |c| reference.score(c),
                    )?;
                    plain.add(&found.report, found.output.len());
                    let mut search = Search::new(target);
                    let seed = table.map(|t| t.seed(target)).expect("checked above");
                    search.seed = Some(seed.quality);
                    search.seed_step = Some(seed.step);
                    let found = search.encode(encoder, &src.image, &params, &registry, |c| {
                        reference.score(c)
                    })?;
                    seeded.add(&found.report, found.output.len());
                    Ok((plain, seeded))
                })
                .try_reduce(
                    || (Tally::default(), Tally::default()),
                    |a, b| Ok((a.0.merge(b.0), a.1.merge(b.1))),
                )?;
            for (mode, t) in [("unseeded", plain), ("seeded", seeded)] {
                let n = t.runs as f64;
                println!(
                    "  {:<15} {:>6.0}  {:<8}  {:>7.2}  {:>8.0}%  {:>6.0}%  {:>7.1}  {:>7.0}",
                    backend.id,
                    target,
                    mode,
                    t.encodes as f64 / n,
                    100.0 * t.first_hit as f64 / n,
                    100.0 * t.reached as f64 / n,
                    t.quality / n,
                    t.bytes / n
                );
            }
            eprintln!("{} at {target}: {:.0?}", backend.id, started.elapsed());
        }
    }
    Ok(())
}
