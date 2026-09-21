//! The grammar of the `sqzer` binary, ADR-0003: one flat command whose
//! flags are a projection of `EncodeParams`, `DecodeOpts` and `Target`.
//! Codec-specific knobs go through `--codec-opt`, never through flags of
//! their own, so a new backend costs this file nothing.

use std::path::PathBuf;

use clap::{ArgAction, Parser, ValueEnum};
use sqzer::core::codec::Format;
use sqzer::core::params::{Preset, Subsampling};

/// The six command shapes of ADR-0001 D5, printed under both help tiers.
const EXAMPLES: &str = "\
Examples:
  sqzer photo.jpg                          photo.avif next to it, content-aware default format
  sqzer photo.jpg -f webp,avif             one input, two outputs
  sqzer ./assets -r -f avif -o ./dist      recurse, mirror the tree into dist
  sqzer *.png --preset lossless -f webp    lossless conversion
  sqzer in.png -t 60 --max-width 1600      lower perceptual target, plus a resize
  sqzer in.png --json                      sizes, scores, chosen params, one line per output
  sqzer --list-codecs                      what this build decodes and encodes, and from which tier

`-h` shows the flags most runs need; `--help` shows every flag.";

/// Command line of the `sqzer` binary.
#[derive(Parser, Debug)]
#[command(
    name = "sqzer",
    version,
    about = "Multi-format image optimizer with best-in-class defaults.",
    long_about = "Multi-format image optimizer with best-in-class defaults.\n\n\
        The default mode is a perceptual target, not a quality slider: encoder quality is \
        searched until the output reaches a SSIMULACRA2 score (70 unless --target says \
        otherwise). Metadata is stripped, ICC is converted to sRGB, EXIF orientation is \
        applied and dropped. Output never overwrites input unless --in-place is given, and an \
        output larger than its input is not written unless --force is.",
    after_help = EXAMPLES,
    after_long_help = EXAMPLES,
    disable_help_subcommand = true
)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Input files, globs, or directories with -r. `-` reads one image
    /// from stdin and writes to stdout.
    ///
    /// An argument that exists on disk is taken literally, never parsed
    /// as a glob. One that does not exist and contains `*`, `?` or `[` is
    /// expanded here, case-insensitively, for shells that do not expand.
    #[arg(value_name = "INPUT")]
    pub inputs: Vec<String>,

    // ---- Output format
    /// Output format(s), comma separated: jpeg, png, webp, avif, jxl.
    /// Default is chosen per image: AVIF for photographs, lossless WebP
    /// for graphics.
    #[arg(short, long, value_name = "FORMAT", value_delimiter = ',', value_parser = parse_format, help_heading = "Output format")]
    pub format: Vec<Format>,

    // ---- Quality
    /// SSIMULACRA2 score to reach by searching encoder quality. Default
    /// 70. 100 is identical, 50 shows artefacts on close inspection.
    #[arg(short, long, value_name = "SCORE", value_parser = parse_target, conflicts_with_all = ["quality", "lossless"], help_heading = "Quality")]
    pub target: Option<f32>,

    /// Encode at this abstract quality, 0 to 100, with no search. Each
    /// backend maps it to its own scale.
    #[arg(short, long, value_name = "0-100", value_parser = parse_quality, conflicts_with = "lossless", help_heading = "Quality")]
    pub quality: Option<f32>,

    /// Lossless output. Refused by encoders that have no lossless mode.
    #[arg(long, help_heading = "Quality")]
    pub lossless: bool,

    /// Effort, 0 to 10. Higher is slower and smaller for every backend.
    #[arg(short, long, value_name = "0-10", value_parser = clap::value_parser!(u8).range(0..=10), help_heading = "Quality")]
    pub effort: Option<u8>,

    /// Named settings: web (target 70), thumbnail (60, fit inside 512 x
    /// 512), archive (85), lossless. Explicit --target, --quality,
    /// --lossless, --effort, --max-width and --max-height override the
    /// preset.
    #[arg(long, value_enum, value_name = "NAME", help_heading = "Quality")]
    pub preset: Option<PresetArg>,

    /// Chroma subsampling for codecs that have it. Refused, not
    /// approximated, by codecs that do not.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        hide_short_help = true,
        help_heading = "Quality"
    )]
    pub subsampling: Option<SubsamplingArg>,

    /// Backend-specific option, repeatable. `--list-codecs -v` prints
    /// every key with its default. Unknown keys are an error.
    #[arg(
        short = 'x',
        long = "codec-opt",
        value_name = "CODEC:KEY=VALUE",
        hide_short_help = true,
        help_heading = "Quality"
    )]
    pub codec_opt: Vec<String>,

    /// Skip the search: encode once at the calibrated seed quality for
    /// the target. Needs a seed table for the backend.
    #[arg(long, conflicts_with_all = ["quality", "lossless"], hide_short_help = true, help_heading = "Quality")]
    pub fast: bool,

    // ---- Resize
    /// Scale down to at most N pixels wide, keeping the aspect ratio.
    /// Never enlarges. The perceptual target is scored against the
    /// resized image.
    ///
    /// Lanczos3 in linear light, alpha premultiplied. EXIF orientation is
    /// applied first, so N bounds the picture as displayed. With
    /// --max-height the image fits inside both. Either flag replaces the
    /// resize of --preset thumbnail.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..), help_heading = "Resize")]
    pub max_width: Option<u32>,

    /// Scale down to at most N pixels tall, keeping the aspect ratio.
    /// Never enlarges.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..), help_heading = "Resize")]
    pub max_height: Option<u32>,

    // ---- Output placement
    /// Output file, when there is one input and one format and PATH has
    /// an extension; output directory otherwise. Default: next to the
    /// input, same stem, new extension.
    #[arg(
        short,
        long,
        value_name = "PATH",
        conflicts_with = "in_place",
        help_heading = "Output placement"
    )]
    pub output: Option<PathBuf>,

    /// Append to the stem: `--suffix -min` writes `photo-min.avif`.
    #[arg(
        long,
        value_name = "TEXT",
        allow_hyphen_values = true,
        conflicts_with = "in_place",
        help_heading = "Output placement"
    )]
    pub suffix: Option<String>,

    /// Output name from placeholders: {stem} {ext} {width} {height}
    /// {format} {quality} {dir} {name}. Example: "{stem}-{width}w.{ext}".
    #[arg(long, value_name = "TEMPLATE", conflicts_with_all = ["in_place", "suffix"], hide_short_help = true, help_heading = "Output placement")]
    pub template: Option<String>,

    /// Write over the input. Only when the format is unchanged.
    #[arg(long, help_heading = "Output placement")]
    pub in_place: bool,

    /// With --in-place, keep the original as `photo@backup.jpg`.
    #[arg(
        long,
        requires = "in_place",
        hide_short_help = true,
        help_heading = "Output placement"
    )]
    pub backup: bool,

    /// Recurse into directories. With -o, the tree under each input
    /// directory is mirrored under the output directory.
    #[arg(short, long, help_heading = "Output placement")]
    pub recursive: bool,

    /// Overwrite an existing output, and write even when the output is
    /// larger than the input.
    #[arg(long, help_heading = "Output placement")]
    pub force: bool,

    // ---- Inputs
    /// Read input paths from FILE, one per line. `-` reads them from
    /// stdin.
    #[arg(
        long,
        value_name = "FILE",
        hide_short_help = true,
        help_heading = "Inputs"
    )]
    pub files_from: Option<PathBuf>,

    /// Paths in --files-from are NUL separated, as `find -print0` and
    /// `fd -0` write them.
    #[arg(
        short = '0',
        long = "null",
        requires = "files_from",
        hide_short_help = true,
        help_heading = "Inputs"
    )]
    pub null: bool,

    /// With -r, only files matching this glob (relative to the input
    /// directory). Repeatable.
    #[arg(
        long,
        value_name = "GLOB",
        hide_short_help = true,
        help_heading = "Inputs"
    )]
    pub include: Vec<String>,

    /// With -r, skip files matching this glob. Repeatable, wins over
    /// --include.
    #[arg(
        long,
        value_name = "GLOB",
        hide_short_help = true,
        help_heading = "Inputs"
    )]
    pub exclude: Vec<String>,

    // ---- Metadata and colour
    /// Keep the ICC profile instead of converting to sRGB.
    #[arg(long, hide_short_help = true, help_heading = "Metadata and colour")]
    pub keep_icc: bool,

    /// Do not apply EXIF orientation while decoding.
    #[arg(long, hide_short_help = true, help_heading = "Metadata and colour")]
    pub no_auto_orient: bool,

    // ---- Resources
    /// Files in flight at once. Default: the number of CPUs. Bounded by
    /// a decoded-pixel budget of --max-pixels times jobs over four, so a
    /// folder of huge images does not exhaust memory.
    #[arg(short, long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..), help_heading = "Resources")]
    pub jobs: Option<u64>,

    /// Refuse to decode an image above this many pixels. Default 268M
    /// (16k x 16k). Accepts k, M and G suffixes.
    #[arg(long, value_name = "N", value_parser = parse_pixels, hide_short_help = true, help_heading = "Resources")]
    pub max_pixels: Option<u64>,

    // ---- Feedback
    /// One JSON object per output on stdout, JSON Lines. Nothing else
    /// goes to stdout.
    #[arg(long, help_heading = "Feedback")]
    pub json: bool,

    /// Decode and plan, encode nothing. Prints each input's dimensions,
    /// alpha, format and the outputs that would be written.
    #[arg(short = 'n', long, help_heading = "Feedback")]
    pub dry_run: bool,

    /// Per-file progress on stderr: auto (when stderr is a terminal),
    /// always, never.
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = When::Auto, hide_short_help = true, help_heading = "Feedback")]
    pub progress: When,

    /// No progress, no warnings. Errors still print.
    #[arg(long, help_heading = "Feedback")]
    pub quiet: bool,

    /// More detail on stderr. -v shows the search trials, -vv the codec
    /// options as resolved.
    #[arg(short, action = ArgAction::Count, help_heading = "Feedback")]
    pub verbose: u8,

    /// Colour on stderr: auto, always, never. `NO_COLOR` is honoured.
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = When::Auto, hide_short_help = true, help_heading = "Feedback")]
    pub color: When,

    /// List what this build decodes and encodes, and from which tier.
    /// With -v, every backend's --codec-opt keys and defaults.
    #[arg(long, help_heading = "Feedback")]
    pub list_codecs: bool,
}

/// `auto` / `always` / `never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum When {
    /// Decide from whether stderr is a terminal.
    Auto,
    /// On.
    Always,
    /// Off.
    Never,
}

impl When {
    /// Resolve against the terminal check.
    pub fn resolve(self, is_terminal: impl FnOnce() -> bool) -> bool {
        match self {
            Self::Auto => is_terminal(),
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// `--preset` values, mirroring [`Preset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PresetArg {
    /// Target 70, effort 6.
    Web,
    /// Target 60, effort 6, fit inside 512 x 512.
    Thumbnail,
    /// Target 85, effort 8.
    Archive,
    /// Lossless, effort 8.
    Lossless,
}

impl From<PresetArg> for Preset {
    fn from(p: PresetArg) -> Self {
        match p {
            PresetArg::Web => Self::Web,
            PresetArg::Thumbnail => Self::Thumbnail,
            PresetArg::Archive => Self::Archive,
            PresetArg::Lossless => Self::Lossless,
        }
    }
}

/// `--subsampling` values, mirroring [`Subsampling`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SubsamplingArg {
    /// The backend decides from the quality.
    Auto,
    /// No chroma subsampling.
    #[value(name = "444")]
    S444,
    /// Horizontal subsampling.
    #[value(name = "422")]
    S422,
    /// Horizontal and vertical subsampling.
    #[value(name = "420")]
    S420,
}

impl From<SubsamplingArg> for Subsampling {
    fn from(s: SubsamplingArg) -> Self {
        match s {
            SubsamplingArg::Auto => Self::Auto,
            SubsamplingArg::S444 => Self::S444,
            SubsamplingArg::S422 => Self::S422,
            SubsamplingArg::S420 => Self::S420,
        }
    }
}

/// Formats with an encoder in some build, as `-f` spells them.
pub const OUTPUT_FORMATS: &[Format] = &[
    Format::Jpeg,
    Format::Png,
    Format::WebP,
    Format::Avif,
    Format::Jxl,
];

/// The name `-f` and `--codec-opt` use for a format.
pub fn format_name(f: Format) -> &'static str {
    match f {
        Format::Jpeg => "jpeg",
        other => other.extension(),
    }
}

fn parse_format(s: &str) -> Result<Format, String> {
    let names = || {
        OUTPUT_FORMATS
            .iter()
            .map(|&f| format_name(f))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match Format::from_extension(s) {
        Some(f) if OUTPUT_FORMATS.contains(&f) => Ok(f),
        Some(f) => Err(format!("{f} is an input format only; one of {}", names())),
        None => Err(format!("unknown format `{s}`; one of {}", names())),
    }
}

fn parse_target(s: &str) -> Result<f32, String> {
    let t: f32 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if !t.is_finite() || t > 100.0 {
        return Err("a SSIMULACRA2 score is a number up to 100".into());
    }
    Ok(t)
}

fn parse_quality(s: &str) -> Result<f32, String> {
    let q: f32 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if !(0.0..=100.0).contains(&q) {
        return Err("quality is 0 to 100".into());
    }
    Ok(q)
}

fn parse_pixels(s: &str) -> Result<u64, String> {
    let (digits, factor) = match s.trim_end_matches(['k', 'K', 'm', 'M', 'g', 'G']) {
        d if d.len() == s.len() => (d, 1u64),
        d => (
            d,
            match s.as_bytes()[s.len() - 1].to_ascii_lowercase() {
                b'k' => 1_000,
                b'm' => 1_000_000,
                _ => 1_000_000_000,
            },
        ),
    };
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("`{s}` is not a pixel count; try 268M"))?;
    n.checked_mul(factor)
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("`{s}` is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("sqzer").chain(args.iter().copied()))
    }

    #[test]
    fn grammar_is_consistent() {
        Args::command().debug_assert();
    }

    #[test]
    fn six_shapes_parse() {
        for shape in [
            "photo.jpg",
            "photo.jpg -f webp,avif,jxl",
            "./assets -r -f avif -o ./dist",
            "a.png b.png --preset lossless -f webp",
            "in.png --target 60",
            "in.png -t 60 --max-width 1600",
            "in.png --json",
            "--list-codecs",
        ] {
            let args: Vec<&str> = shape.split(' ').collect();
            parse(&args).unwrap_or_else(|e| panic!("{shape}: {e}"));
        }
        let a = parse(&["photo.jpg", "-f", "webp,avif,jxl"]).unwrap();
        assert_eq!(a.format, vec![Format::WebP, Format::Avif, Format::Jxl]);
    }

    #[test]
    fn flag_order_does_not_matter() {
        let a = parse(&["-q", "80", "a.png", "-f", "png", "b.png", "-e", "3"]).unwrap();
        assert_eq!(a.inputs, vec!["a.png", "b.png"]);
        assert_eq!(a.quality, Some(80.0));
        assert_eq!(a.effort, Some(3));
    }

    #[test]
    fn exclusive_targets_are_usage_errors() {
        for bad in [
            &["a.png", "--target", "70", "--quality", "80"][..],
            &["a.png", "--quality", "80", "--lossless"],
            &["a.png", "--target", "70", "--lossless"],
            &["a.png", "--fast", "-q", "50"],
            &["a.png", "--in-place", "-o", "out"],
            &["a.png", "--backup"],
            &["a.png", "-0"],
            &["a.png", "-f", "gif"],
            &["a.png", "-f", "bmp"],
            &["a.png", "-e", "11"],
            &["a.png", "-q", "101"],
            &["a.png", "-t", "150"],
            &["a.png", "-j", "0"],
            &["a.png", "--max-pixels", "0"],
            &["a.png", "--max-width", "0"],
            &["a.png", "--max-height", "0"],
            &["a.png", "--max-width", "wide"],
        ] {
            let err = parse(bad).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{bad:?}: {err}");
        }
    }

    #[test]
    fn short_flags_mean_what_the_adr_says() {
        let a = parse(&[
            "a.png",
            "-t",
            "65",
            "-e",
            "9",
            "-j",
            "2",
            "-n",
            "-vv",
            "-x",
            "jpeg:progressive=false",
        ])
        .unwrap();
        assert_eq!(a.target, Some(65.0));
        assert_eq!(a.effort, Some(9));
        assert_eq!(a.jobs, Some(2));
        assert!(a.dry_run);
        assert_eq!(a.verbose, 2);
        assert_eq!(a.codec_opt, vec!["jpeg:progressive=false"]);
        // `-q` is quality, never quiet.
        let a = parse(&["a.png", "-q", "70"]).unwrap();
        assert_eq!(a.quality, Some(70.0));
        assert!(!a.quiet);
        // A suffix usually starts with a hyphen.
        let a = parse(&["a.png", "--suffix", "-min"]).unwrap();
        assert_eq!(a.suffix.as_deref(), Some("-min"));
    }

    #[test]
    fn pixel_counts_take_suffixes() {
        assert_eq!(parse_pixels("268435456").unwrap(), 268_435_456);
        assert_eq!(parse_pixels("268M").unwrap(), 268_000_000);
        assert_eq!(parse_pixels("2k").unwrap(), 2_000);
        assert_eq!(parse_pixels("1G").unwrap(), 1_000_000_000);
        assert!(parse_pixels("lots").is_err());
        assert!(parse_pixels("0").is_err());
    }

    #[test]
    fn format_names_round_trip() {
        for &f in OUTPUT_FORMATS {
            assert_eq!(parse_format(format_name(f)).unwrap(), f);
            assert_eq!(parse_format(&format_name(f).to_uppercase()).unwrap(), f);
        }
        assert_eq!(parse_format("jpg").unwrap(), Format::Jpeg);
    }
}
