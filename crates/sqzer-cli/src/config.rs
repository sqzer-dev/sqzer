//! From parsed arguments to a checked run configuration. Everything that
//! can be refused before a file is touched is refused here: exit 2 for an
//! argument error, exit 3 when the build cannot do what was asked.

use std::io::IsTerminal;
use std::path::PathBuf;

use glob::Pattern;
use sqzer::Sqzer;
use sqzer::core::codec::Format;
use sqzer::core::params::{Resize, Target};
use sqzer::core::resize::{Fit, Size};

use crate::budget::Work;
use crate::cli::{Args, OUTPUT_FORMATS, format_name};
use crate::inputs::Input;
use crate::output::{Placement, Template};
use crate::report::{Feedback, Need, render_unavailable};

/// A refusal with its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// 2 for an argument error, 3 when nothing could be done.
    pub code: u8,
    /// One or more lines, without the `error:` prefix.
    pub message: String,
}

impl Failure {
    /// Exit 2.
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    /// Exit 3.
    pub fn nothing(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
        }
    }
}

/// The checked run.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    /// Positional arguments as given.
    pub inputs: Vec<String>,
    /// `--files-from`.
    pub files_from: Option<PathBuf>,
    /// `-0`.
    pub null: bool,
    /// `-r`.
    pub recursive: bool,
    /// `--include`.
    pub include: Vec<Pattern>,
    /// `--exclude`.
    pub exclude: Vec<Pattern>,
    /// `-f`. Empty means the content-aware default, one output per input.
    pub formats: Vec<Format>,
    /// A `--width` list of more than one width, each an output of its
    /// own. Empty for one size, which is the resize on `sqzer`.
    pub widths: Vec<u32>,
    /// The library builder, fully configured except for the format.
    pub sqzer: Sqzer,
    /// Where outputs go.
    pub placement: Placement,
    /// `--backup`.
    pub backup: bool,
    /// `--force`.
    pub force: bool,
    /// `-n`.
    pub dry_run: bool,
    /// `-j`.
    pub jobs: usize,
    /// `--max-pixels`.
    pub max_pixels: u64,
    /// What each file goes through, which sizes its memory reservation.
    pub work: Work,
    /// The feedback flags.
    pub feedback: Feedback,
}

/// Check `args` against the registry inside `base`.
///
/// # Errors
/// [`Failure`] with exit code 2 or 3.
pub fn build(args: Args, base: Sqzer) -> Result<Config, Failure> {
    if args.inputs.is_empty() && args.files_from.is_none() {
        return Err(Failure::usage(
            "no input given; pass files, globs or a directory with `-r`. `sqzer -h` shows the flags",
        ));
    }
    let sqzer = quality_flags(&args, base);
    let sqzer = resize_flags(&args, sqzer)?;
    check_formats(&args, &sqzer)?;
    if let Some(f) = args
        .format
        .iter()
        .enumerate()
        .find_map(|(i, f)| args.format[..i].contains(f).then_some(f))
    {
        return Err(Failure::usage(format!(
            "`-f` names {} twice; each format is written once",
            format_name(*f)
        )));
    }
    let widths = if args.width.len() > 1 {
        args.width.clone()
    } else {
        Vec::new()
    };
    if !widths.is_empty() && args.in_place {
        return Err(Failure::usage(
            "`--in-place` writes one output, and a `--width` list asks for several",
        ));
    }
    let sqzer = codec_opts(&args, sqzer)?;

    let template = match &args.template {
        Some(t) => Some(Template::new(t).map_err(Failure::usage)?),
        None => None,
    };
    if let Some(t) = &template {
        if !widths.is_empty() && !t.has("width") {
            return Err(Failure::usage(
                "a `--width` list writes one file per width, and the `--template` has no \
                 `{width}` to tell them apart",
            ));
        }
        if args.format.len() > 1 && !t.has("ext") && !t.has("format") {
            return Err(Failure::usage(
                "several `-f` formats write one file per format, and the `--template` has \
                 neither `{ext}` nor `{format}` to tell them apart",
            ));
        }
    }
    let placement = Placement {
        output: args.output.clone(),
        single_file: false,
        suffix: args.suffix.clone(),
        template,
        in_place: args.in_place,
        width_suffix: !widths.is_empty(),
    };

    let pattern = |g: &String| {
        Pattern::new(g).map_err(|e| Failure::usage(format!("`{g}` is not a valid glob: {e}")))
    };
    let include = args.include.iter().map(pattern).collect::<Result<_, _>>()?;
    let exclude = args.exclude.iter().map(pattern).collect::<Result<_, _>>()?;

    let jobs = args.jobs.map_or_else(
        || std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        |j| usize::try_from(j).unwrap_or(usize::MAX),
    );
    let max_pixels = sqzer.decode_opts().max_pixels;
    // A perceptual target searches unless `--fast` takes the seed. A
    // format picked per image may turn out lossless-only and skip the
    // search; the estimate stays on the safe side of that.
    let work = if args.dry_run {
        Work::Plan
    } else if matches!(sqzer.params().target, Target::Ssimulacra2(_)) && !args.fast {
        Work::Search
    } else {
        Work::Encode
    };

    let feedback = Feedback {
        json: args.json,
        progress: !args.quiet && args.progress.resolve(|| std::io::stderr().is_terminal()),
        quiet: args.quiet,
        verbose: args.verbose,
        name_width: 0,
    };

    Ok(Config {
        inputs: args.inputs,
        files_from: args.files_from,
        null: args.null,
        recursive: args.recursive,
        include,
        exclude,
        formats: args.format,
        widths,
        sqzer,
        placement,
        backup: args.backup,
        force: args.force,
        dry_run: args.dry_run,
        jobs,
        max_pixels,
        work,
        feedback,
    })
}

/// Preset first, then the flags that override it.
fn quality_flags(args: &Args, mut sqzer: Sqzer) -> Sqzer {
    if let Some(p) = args.preset {
        sqzer = sqzer.preset(p.into());
    }
    if let Some(t) = args.target {
        sqzer = sqzer.target(Target::Ssimulacra2(t));
    }
    if let Some(q) = args.quality {
        sqzer = sqzer.target(Target::Quality(q));
    }
    if args.lossless {
        sqzer = sqzer.target(Target::Lossless);
    }
    if let Some(e) = args.effort {
        sqzer = sqzer.effort(e);
    }
    if let Some(s) = args.subsampling {
        sqzer = sqzer.subsampling(s.into());
    }
    if let Some(n) = args.max_pixels {
        sqzer = sqzer.max_pixels(n);
    }
    sqzer
        .keep_icc(args.keep_icc)
        .keep_metadata(args.keep_metadata)
        .auto_orient(!args.no_auto_orient)
        .fast(args.fast)
}

/// The resize flags of ADR-0009 D1 over the preset's resize, with the
/// rules the parser cannot express. A size flag replaces the preset's box
/// whole: `--preset thumbnail --max-width 1600` means 1600 wide, not 1600
/// wide inside 512 tall. The other flags modify whatever box there is, so
/// `--preset thumbnail --fit cover` crops to 512 x 512.
fn resize_flags(args: &Args, sqzer: Sqzer) -> Result<Sqzer, Failure> {
    let mut r = sqzer.resize_bounds();
    let size = if let Some(f) = args.scale {
        Some(Size::Scale(f))
    } else if args.max_width.is_some() || args.max_height.is_some() {
        Some(Size::Box {
            width: args.max_width,
            height: args.max_height,
        })
    } else if !args.width.is_empty() || args.height.is_some() {
        Some(Size::Box {
            width: args.width.first().copied(),
            height: args.height,
        })
    } else {
        None
    };
    if let Some(size) = size {
        r = Resize {
            size,
            ..Resize::NONE
        };
    }
    if let Some(f) = args.fit {
        r.fit = f.into();
    }
    if let Some(p) = args.position {
        r.position = p.into();
    }
    r.background = args.background.or(r.background);
    r.enlarge |= args.enlarge;
    if let Some(f) = args.filter {
        r.filter = f.into();
    }

    if let Some(w) = args
        .width
        .iter()
        .enumerate()
        .find_map(|(i, w)| args.width[..i].contains(w).then_some(w))
    {
        return Err(Failure::usage(format!("`--width` lists {w} twice")));
    }
    if let Size::Box { width, height } = r.size
        && r.fit != Fit::Inside
        && (width.is_none() || height.is_none())
    {
        return Err(Failure::usage(format!(
            "`--fit {}` needs both `--width` and `--height`",
            r.fit.name()
        )));
    }
    if args.position.is_some() && !matches!(r.fit, Fit::Cover | Fit::Contain) {
        return Err(Failure::usage(
            "`--position` places a `--fit cover` crop or a `--fit contain` image, and neither \
             is set",
        ));
    }
    if args.background.is_some() && r.fit != Fit::Contain {
        return Err(Failure::usage(
            "`--background` pads a `--fit contain` image, and `--fit` is not `contain`",
        ));
    }
    r.check().map_err(|e| match e {
        sqzer::core::Error::InvalidParams(m) => Failure::usage(m),
        other => Failure::usage(other.to_string()),
    })?;
    Ok(sqzer.resize(r))
}

/// Every requested format must have an encoder that can do the requested
/// mode, else nothing can be done: exit 3.
fn check_formats(args: &Args, sqzer: &Sqzer) -> Result<(), Failure> {
    let registry = sqzer.registry();
    let target = &sqzer.params().target;
    let resize = sqzer.resize_bounds();
    let translucent =
        resize.fit == Fit::Contain && resize.background.is_some_and(|b| b[3] < u8::MAX);
    for &f in &args.format {
        let enc = registry
            .encoder(f)
            .map_err(|_| Failure::nothing(render_unavailable(f, Need::Any, registry)))?;
        let caps = enc.caps();
        if translucent && !caps.alpha {
            return Err(Failure::nothing(format!(
                "{} has no alpha channel for a translucent `--background` (ADR-0010 D3)\n  pick \
                 an opaque colour, or a format with alpha",
                caps.name
            )));
        }
        match target {
            Target::Quality(_) if !caps.lossy => {
                return Err(Failure::nothing(render_unavailable(
                    f,
                    Need::Lossy,
                    registry,
                )));
            }
            Target::Lossless if !caps.lossless => {
                return Err(Failure::nothing(render_unavailable(
                    f,
                    Need::Lossless,
                    registry,
                )));
            }
            Target::Ssimulacra2(_) if caps.lossy && !registry.has_decoder(f) => {
                return Err(Failure::nothing(format!(
                    "a perceptual target needs a {f} decoder to score the output, and this \
                     build has none\n  pass -q for an explicit quality, or --lossless"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

/// `--codec-opt` keys are checked against the backend that owns them.
fn codec_opts(args: &Args, mut sqzer: Sqzer) -> Result<Sqzer, Failure> {
    for opt in &args.codec_opt {
        let (codec, key, value) = split_codec_opt(opt)?;
        let format = Format::from_extension(codec)
            .filter(|f| OUTPUT_FORMATS.contains(f))
            .ok_or_else(|| {
                Failure::usage(format!(
                    "`{opt}`: unknown codec `{codec}`; one of {}",
                    OUTPUT_FORMATS
                        .iter()
                        .map(|&f| format_name(f))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        let codec = format_name(format);
        let enc = sqzer.registry().encoder(format).map_err(|_| {
            Failure::usage(format!(
                "`{opt}`: no {format} encoder in this build to take it; `sqzer --list-codecs` \
                 shows what there is"
            ))
        })?;
        let caps = enc.caps();
        if !caps.options.iter().any(|o| o.key == key) {
            let known: Vec<String> = caps
                .options
                .iter()
                .map(|o| format!("{codec}:{}", o.key))
                .collect();
            let known = if known.is_empty() {
                format!("{} takes no options", caps.name)
            } else {
                format!("{} accepts {}", caps.name, known.join(", "))
            };
            return Err(Failure::usage(format!(
                "`{opt}`: unknown {codec} option `{key}`; {known}. `sqzer --list-codecs -v` \
                 describes each"
            )));
        }
        sqzer = sqzer.codec_opt(codec, key, value);
    }
    Ok(sqzer)
}

impl Config {
    /// Checks that need the resolved inputs: whether `-o` names a file,
    /// and the stdin rules.
    ///
    /// # Errors
    /// [`Failure`] with exit code 2.
    pub fn finish(&mut self, inputs: &[Input]) -> Result<(), Failure> {
        let stdin = inputs.iter().filter(|i| **i == Input::Stdin).count();
        if stdin > 0 && !self.widths.is_empty() && self.placement.output.is_none() {
            return Err(Failure::usage(
                "a `--width` list writes one image per width, and stdout takes one; pass `-o` \
                 with a directory",
            ));
        }
        if stdin > 0 {
            if self.formats.len() != 1 {
                return Err(Failure::usage(
                    "reading stdin needs exactly one `-f`, there is no file name to infer from",
                ));
            }
            if self.placement.in_place {
                return Err(Failure::usage("`--in-place` has no meaning for stdin"));
            }
            if self.placement.output.is_none() && self.feedback.json && !self.dry_run {
                return Err(Failure::usage(
                    "`--json` and the image cannot both go to stdout; pass `-o` for the image",
                ));
            }
        }
        self.placement.single_file = self.placement.output.as_deref().is_some_and(|o| {
            o.extension().is_some() && !o.is_dir() && inputs.len() == 1 && self.formats.len() <= 1
        });
        if self.placement.single_file && !self.widths.is_empty() {
            return Err(Failure::usage(format!(
                "`-o {}` names one file, and a `--width` list writes one per width; pass a \
                 directory",
                self.placement
                    .output
                    .as_deref()
                    .map(|o| o.display().to_string())
                    .unwrap_or_default()
            )));
        }
        self.feedback.name_width = inputs
            .iter()
            .map(|i| i.display().chars().count())
            .max()
            .unwrap_or(0);
        Ok(())
    }
}

/// `codec:key=value`.
fn split_codec_opt(opt: &str) -> Result<(&str, &str, &str), Failure> {
    let bad = || {
        Failure::usage(format!(
            "`{opt}`: a codec option is `codec:key=value`, for example `jpeg:progressive=false`"
        ))
    };
    let (codec, rest) = opt.split_once(':').ok_or_else(bad)?;
    let (key, value) = rest.split_once('=').ok_or_else(bad)?;
    if codec.is_empty() || key.is_empty() {
        return Err(bad());
    }
    Ok((codec, key, value))
}

#[cfg(all(test, feature = "portable"))]
mod tests {
    use super::*;
    use clap::Parser;
    use sqzer::core::params::Subsampling;

    fn build_from(args: &[&str]) -> Result<Config, Failure> {
        let args =
            Args::try_parse_from(std::iter::once("sqzer").chain(args.iter().copied())).unwrap();
        build(args, Sqzer::new())
    }

    #[test]
    fn flags_reach_encode_params_exactly() {
        let cfg = build_from(&[
            "a.png",
            "-q",
            "82",
            "-e",
            "9",
            "--subsampling",
            "420",
            "--keep-icc",
            "--keep-metadata",
            "--no-auto-orient",
            "--max-pixels",
            "1M",
            // Options that exist in both tiers: PNG stays portable, and
            // both AVIF backends take `alpha_quality`.
            "-x",
            "png:interlace=true",
            "-x",
            "avif:alpha_quality=50",
        ])
        .unwrap();
        let p = cfg.sqzer.params();
        assert_eq!(p.target, Target::Quality(82.0));
        assert_eq!(p.effort, 9);
        assert_eq!(p.subsampling, Subsampling::S420);
        assert!(p.keep_icc);
        assert!(p.keep_metadata);
        assert_eq!(
            p.codec_specific.get("png:interlace").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            p.codec_specific
                .get("avif:alpha_quality")
                .map(String::as_str),
            Some("50")
        );
        assert!(!cfg.sqzer.decode_opts().apply_orientation);
        assert_eq!(cfg.max_pixels, 1_000_000);
        // Presets and their overrides.
        let cfg = build_from(&["a.png", "--preset", "archive"]).unwrap();
        assert_eq!(cfg.sqzer.params().target, Target::Ssimulacra2(85.0));
        assert_eq!(cfg.sqzer.params().effort, 8);
        let cfg = build_from(&["a.png", "--preset", "archive", "-t", "60", "-e", "1"]).unwrap();
        assert_eq!(cfg.sqzer.params().target, Target::Ssimulacra2(60.0));
        assert_eq!(cfg.sqzer.params().effort, 1);
        let cfg = build_from(&["a.png", "--lossless"]).unwrap();
        assert_eq!(cfg.sqzer.params().target, Target::Lossless);
    }

    #[test]
    fn only_a_searched_target_is_costed_as_a_search() {
        let work = |args: &[&str]| build_from(args).unwrap().work;
        assert_eq!(work(&["a.png"]), Work::Search);
        assert_eq!(work(&["a.png", "-t", "80"]), Work::Search);
        assert_eq!(work(&["a.png", "--fast"]), Work::Encode);
        assert_eq!(work(&["a.png", "-q", "80"]), Work::Encode);
        assert_eq!(work(&["a.png", "--lossless"]), Work::Encode);
        assert_eq!(work(&["a.png", "-n"]), Work::Plan);
    }

    #[test]
    fn resize_flags_reach_the_builder_and_replace_the_preset_box() {
        let bounds = |args: &[&str]| match build_from(args).unwrap().sqzer.resize_bounds().size {
            Size::Box { width, height } => (width, height),
            Size::Scale(_) => panic!("{args:?}"),
        };
        assert_eq!(bounds(&["a.png"]), (None, None));
        assert_eq!(
            bounds(&["a.png", "--max-width", "1600"]),
            (Some(1600), None)
        );
        assert_eq!(
            bounds(&["a.png", "--max-width", "1600", "--max-height", "900"]),
            (Some(1600), Some(900))
        );
        assert_eq!(
            bounds(&["a.png", "--preset", "thumbnail"]),
            (Some(512), Some(512))
        );
        assert_eq!(
            bounds(&["a.png", "--preset", "thumbnail", "--max-height", "128"]),
            (None, Some(128))
        );
    }

    #[test]
    fn resize_flags_build_the_request() {
        use sqzer::core::resize::{Filter, Position};
        let resize = |args: &[&str]| build_from(args).unwrap().sqzer.resize_bounds();
        let r = resize(&[
            "a.png",
            "--width",
            "400",
            "--height",
            "300",
            "--fit",
            "contain",
            "--position",
            "bottom",
            "--background",
            "#fff",
            "--filter",
            "nearest",
            "--enlarge",
        ]);
        assert_eq!(
            r,
            Resize {
                size: Size::Box {
                    width: Some(400),
                    height: Some(300),
                },
                fit: Fit::Contain,
                position: Position::Bottom,
                background: Some([255; 4]),
                enlarge: true,
                filter: Filter::Nearest,
            }
        );
        assert_eq!(resize(&["a.png", "--scale", "50%"]).size, Size::Scale(0.5));
        // The preset's box takes a fit; a size flag replaces the box.
        let r = resize(&["a.png", "--preset", "thumbnail", "--fit", "cover"]);
        assert_eq!(r.fit, Fit::Cover);
        assert_eq!(r.size, Resize::inside(Some(512), Some(512)).size);
        let r = resize(&["a.png", "--preset", "thumbnail", "--width", "800"]);
        assert_eq!(r, Resize::inside(Some(800), None));
        // A width list keeps its widths for the job, the first on the
        // builder.
        let cfg = build_from(&["a.png", "--width", "480,960"]).unwrap();
        assert_eq!(cfg.widths, vec![480, 960]);
        assert!(cfg.placement.width_suffix);
        assert_eq!(cfg.sqzer.resize_bounds(), Resize::inside(Some(480), None));
        let cfg = build_from(&["a.png", "--width", "480"]).unwrap();
        assert!(cfg.widths.is_empty());
        assert!(!cfg.placement.width_suffix);
    }

    #[test]
    fn resize_rules_are_usage_errors() {
        for (bad, needle) in [
            (
                &["a.png", "--width", "10", "--fit", "cover"][..],
                "needs both",
            ),
            (&["a.png", "--height", "10", "--fit", "fill"], "needs both"),
            (&["a.png", "--fit", "contain"], "needs both"),
            (
                &["a.png", "--width", "10", "--position", "top"],
                "--position",
            ),
            (
                &[
                    "a.png",
                    "--width",
                    "1",
                    "--height",
                    "1",
                    "--fit",
                    "cover",
                    "--background",
                    "white",
                ],
                "--background",
            ),
            (&["a.png", "--width", "480,960,480"], "480 twice"),
            (
                &["a.png", "--width", "480,960", "--template", "{stem}.{ext}"],
                "{width}",
            ),
            (&["a.png", "--width", "480,960", "--in-place"], "--in-place"),
            (
                &["a.png", "-f", "png,webp", "--template", "{stem}"],
                "{ext}",
            ),
            (&["a.png", "-f", "png,png"], "png twice"),
            (
                &["a.png", "--width", "10", "--fit", "outside"],
                "needs both",
            ),
            (
                &[
                    "a.png",
                    "--width",
                    "1",
                    "--height",
                    "1",
                    "--fit",
                    "outside",
                    "--position",
                    "top",
                ],
                "--position",
            ),
        ] {
            let err = build_from(bad).unwrap_err();
            assert_eq!(err.code, 2, "{bad:?}");
            assert!(err.message.contains(needle), "{bad:?}: {}", err.message);
        }
        // A translucent padding for a format without alpha: nothing can be
        // done. An opaque one, or a format with alpha, is fine.
        let contain = ["a.png", "--width", "8", "--height", "8", "--fit", "contain"];
        let with = |extra: &[&'static str]| {
            let mut args = contain.to_vec();
            args.extend_from_slice(extra);
            build_from(&args)
        };
        let err = with(&["-f", "jpeg", "--background", "#ffffff80"]).unwrap_err();
        assert_eq!(err.code, 3);
        assert!(err.message.contains("translucent"), "{}", err.message);
        assert!(with(&["-f", "jpeg", "--background", "#ffffff"]).is_ok());
        assert!(with(&["-f", "png", "--background", "transparent"]).is_ok());
        // A width list needs somewhere to put several files.
        let mut cfg = build_from(&["a.png", "--width", "480,960", "-o", "b.avif"]).unwrap();
        let err = cfg.finish(&[Input::File("a.png".into())]).unwrap_err();
        assert!(err.message.contains("names one file"), "{}", err.message);
        let mut cfg = build_from(&["-", "-f", "png", "--width", "480,960"]).unwrap();
        let err = cfg.finish(&[Input::Stdin]).unwrap_err();
        assert!(err.message.contains("stdout"), "{}", err.message);
        let mut cfg = build_from(&["-", "-f", "png", "--width", "480,960", "-o", "out"]).unwrap();
        assert!(cfg.finish(&[Input::Stdin]).is_ok());
    }

    #[test]
    fn unknown_codec_opts_are_usage_errors() {
        // A build with `gamut-jxl` rejects the key itself; one without
        // has no encoder to ask.
        let jxl_needle = if crate::native_set::JXL {
            "unknown jxl option `effort`"
        } else {
            "no JPEG XL encoder in this build"
        };
        for (bad, needle) in [
            ("jpeg:nope=1", "unknown jpeg option `nope`"),
            ("jpeg:nope", "codec:key=value"),
            ("bmp:x=1", "unknown codec `bmp`"),
            ("jxl:effort=7", jxl_needle),
        ] {
            let err = build_from(&["a.png", "-x", bad]).unwrap_err();
            assert_eq!(err.code, 2, "{bad}");
            assert!(err.message.contains(needle), "{bad}: {}", err.message);
        }
    }

    #[test]
    fn a_format_this_build_cannot_write_is_exit_3() {
        // A build with `gamut-jxl` writes it; the others refuse.
        if !crate::native_set::JXL {
            let err = build_from(&["a.png", "-f", "jxl"]).unwrap_err();
            assert_eq!(err.code, 3);
            assert!(
                err.message.contains("no JPEG XL encoder"),
                "{}",
                err.message
            );
            // A native build on musl explains the gap; a portable build
            // names the feature and the release.
            if crate::native_set::NATIVE {
                assert!(err.message.contains("musl"), "{}", err.message);
                assert!(!err.message.contains("releases page"), "{}", err.message);
            } else {
                assert!(err.message.contains("releases page"), "{}", err.message);
            }
        }
        // Lossy WebP is `webpx`, which every `native` build has.
        if !cfg!(feature = "native") {
            let err = build_from(&["a.png", "-f", "webp", "-q", "80"]).unwrap_err();
            assert_eq!(err.code, 3);
            assert!(err.message.contains("for lossy output"), "{}", err.message);
        }
        let err = build_from(&["a.png", "-f", "jpeg", "--lossless"]).unwrap_err();
        assert_eq!(err.code, 3);
        assert!(
            err.message.contains("for lossless output"),
            "{}",
            err.message
        );
        // Lossless WebP under a perceptual target is fine: it meets it.
        assert!(build_from(&["a.png", "-f", "webp"]).is_ok());
    }

    #[test]
    fn no_input_is_a_usage_error() {
        let err = build_from(&["-q", "80"]).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(build_from(&["--files-from", "list.txt"]).is_ok());
    }

    #[test]
    fn stdin_rules_and_single_file_output() {
        let mut cfg = build_from(&["-", "-f", "png"]).unwrap();
        assert!(cfg.finish(&[Input::Stdin]).is_ok());
        let mut cfg = build_from(&["-"]).unwrap();
        assert_eq!(cfg.finish(&[Input::Stdin]).unwrap_err().code, 2);
        let mut cfg = build_from(&["-", "-f", "png", "--json"]).unwrap();
        assert_eq!(cfg.finish(&[Input::Stdin]).unwrap_err().code, 2);
        let mut cfg = build_from(&["-", "-f", "png", "--json", "-o", "out.png"]).unwrap();
        assert!(cfg.finish(&[Input::Stdin]).is_ok());
        assert!(cfg.placement.single_file);

        let mut cfg = build_from(&["a.png", "-o", "b.avif"]).unwrap();
        cfg.finish(&[Input::File("a.png".into())]).unwrap();
        assert!(cfg.placement.single_file);
        let mut cfg = build_from(&["a.png", "c.png", "-o", "b.avif"]).unwrap();
        cfg.finish(&[Input::File("a.png".into()), Input::File("c.png".into())])
            .unwrap();
        assert!(!cfg.placement.single_file);
        let mut cfg = build_from(&["a.png", "-o", "b.avif", "-f", "avif,png"]).unwrap();
        cfg.finish(&[Input::File("a.png".into())]).unwrap();
        assert!(!cfg.placement.single_file);
    }
}
