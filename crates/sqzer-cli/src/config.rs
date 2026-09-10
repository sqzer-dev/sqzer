//! From parsed arguments to a checked run configuration. Everything that
//! can be refused before a file is touched is refused here: exit 2 for an
//! argument error, exit 3 when the build cannot do what was asked.

use std::io::IsTerminal;
use std::path::PathBuf;

use glob::Pattern;
use sqzer::Sqzer;
use sqzer::core::codec::Format;
use sqzer::core::params::Target;

use crate::cli::{Args, OUTPUT_FORMATS, When, format_name};
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
            "no input given; pass files, globs or a directory with -r. `sqzer -h` shows the flags",
        ));
    }
    let sqzer = quality_flags(&args, base);
    check_formats(&args, &sqzer)?;
    let sqzer = codec_opts(&args, sqzer)?;

    let template = match &args.template {
        Some(t) => Some(Template::new(t).map_err(Failure::usage)?),
        None => None,
    };
    let placement = Placement {
        output: args.output.clone(),
        single_file: false,
        suffix: args.suffix.clone(),
        template,
        in_place: args.in_place,
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

    let stderr_tty = || std::io::stderr().is_terminal();
    let feedback = Feedback {
        json: args.json,
        progress: !args.quiet && args.progress.resolve(stderr_tty),
        quiet: args.quiet,
        verbose: args.verbose,
        color: match args.color {
            When::Always => true,
            When::Never => false,
            When::Auto => stderr_tty() && !no_color(),
        },
    };

    Ok(Config {
        inputs: args.inputs,
        files_from: args.files_from,
        null: args.null,
        recursive: args.recursive,
        include,
        exclude,
        formats: args.format,
        sqzer,
        placement,
        backup: args.backup,
        force: args.force,
        dry_run: args.dry_run,
        jobs,
        max_pixels,
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
        .auto_orient(!args.no_auto_orient)
        .fast(args.fast)
}

/// Every requested format must have an encoder that can do the requested
/// mode, else nothing can be done: exit 3.
fn check_formats(args: &Args, sqzer: &Sqzer) -> Result<(), Failure> {
    let registry = sqzer.registry();
    let target = &sqzer.params().target;
    for &f in &args.format {
        let enc = registry
            .encoder(f)
            .map_err(|_| Failure::nothing(render_unavailable(f, Need::Any, registry)))?;
        let caps = enc.caps();
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
        if stdin > 0 {
            if self.formats.len() != 1 {
                return Err(Failure::usage(
                    "reading stdin needs exactly one -f, there is no file name to infer from",
                ));
            }
            if self.placement.in_place {
                return Err(Failure::usage("--in-place has no meaning for stdin"));
            }
            if self.placement.output.is_none() && self.feedback.json && !self.dry_run {
                return Err(Failure::usage(
                    "--json and the image cannot both go to stdout; pass -o for the image",
                ));
            }
        }
        self.placement.single_file = self.placement.output.as_deref().is_some_and(|o| {
            o.extension().is_some() && !o.is_dir() && inputs.len() == 1 && self.formats.len() <= 1
        });
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

/// The `NO_COLOR` convention: set and non-empty means no colour.
fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
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
            "--no-auto-orient",
            "--max-pixels",
            "1M",
            "-x",
            "jpeg:progressive=false",
            "-x",
            "avif:bit_depth=8",
        ])
        .unwrap();
        let p = cfg.sqzer.params();
        assert_eq!(p.target, Target::Quality(82.0));
        assert_eq!(p.effort, 9);
        assert_eq!(p.subsampling, Subsampling::S420);
        assert!(p.keep_icc);
        assert_eq!(
            p.codec_specific.get("jpeg:progressive").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            p.codec_specific.get("avif:bit_depth").map(String::as_str),
            Some("8")
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
    fn unknown_codec_opts_are_usage_errors() {
        for (bad, needle) in [
            ("jpeg:nope=1", "unknown jpeg option `nope`"),
            ("jpeg:progressive", "codec:key=value"),
            ("bmp:x=1", "unknown codec `bmp`"),
            ("jxl:effort=7", "no JPEG XL encoder in this build"),
        ] {
            let err = build_from(&["a.png", "-x", bad]).unwrap_err();
            assert_eq!(err.code, 2, "{bad}");
            assert!(err.message.contains(needle), "{bad}: {}", err.message);
        }
    }

    #[test]
    fn a_format_this_build_cannot_write_is_exit_3() {
        let err = build_from(&["a.png", "-f", "jxl"]).unwrap_err();
        assert_eq!(err.code, 3);
        assert!(
            err.message.contains("no JPEG XL encoder"),
            "{}",
            err.message
        );
        let err = build_from(&["a.png", "-f", "webp", "-q", "80"]).unwrap_err();
        assert_eq!(err.code, 3);
        assert!(err.message.contains("for lossy output"), "{}", err.message);
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
