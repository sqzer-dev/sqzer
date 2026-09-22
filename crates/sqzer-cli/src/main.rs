//! `sqzer` binary. The grammar is ADR-0003; this file only wires the
//! modules together and turns the outcome into an exit code:
//!
//! ```text
//! 0   every input produced every requested output (skipped-as-larger counts)
//! 1   at least one input failed
//! 2   argument error
//! 3   nothing could be done: no input matched, or no encoder for the format
//! ```

mod budget;
mod cli;
mod codecs;
mod config;
mod inputs;
mod job;
mod native_set;
mod output;
mod report;

use std::path::Path;
use std::process::ExitCode;

use clap::{ColorChoice, CommandFactory, FromArgMatches};
use rayon::prelude::*;
use sqzer::Sqzer;
use sqzer::core::codec::Format;

use budget::PixelBudget;
use cli::{Args, When};
use config::Failure;
use inputs::Input;
use report::{Printer, Tally};

fn main() -> ExitCode {
    let raw_args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let color = color_choice(&raw_args);
    anstream::ColorChoice::write_global(match color {
        ColorChoice::Always => anstream::ColorChoice::Always,
        ColorChoice::Never => anstream::ColorChoice::Never,
        ColorChoice::Auto => anstream::ColorChoice::Auto,
    });
    // The progress bars draw through `console`, which decides on its own
    // from the terminal and `NO_COLOR`; only an explicit choice overrides.
    match color {
        ColorChoice::Always => console::set_colors_enabled_stderr(true),
        ColorChoice::Never => console::set_colors_enabled_stderr(false),
        ColorChoice::Auto => {}
    }

    // `sqzer mozjpeg -q 75 in.jpg` is rimage syntax; say so before clap
    // complains about the flags that follow.
    if let Some(hint) = inputs::rimage_hint(&raw_args) {
        report::error_line(&hint);
        return ExitCode::from(2);
    }

    let matches = match Args::command()
        .color(color)
        .try_get_matches_from(std::iter::once("sqzer".to_string()).chain(raw_args))
    {
        Ok(m) => m,
        // Prints help or version and exits 0, or a usage error and exits 2.
        Err(e) => e.exit(),
    };
    let args = match Args::from_arg_matches(&matches) {
        Ok(a) => a,
        Err(e) => e.exit(),
    };
    match run(args) {
        Ok(code) => code,
        Err(f) => {
            report::error_line(&f.message);
            ExitCode::from(f.code)
        }
    }
}

fn run(args: Args) -> Result<ExitCode, Failure> {
    let base = Sqzer::new();
    if args.list_codecs {
        if args.json {
            println!("{}", codecs::render_json(base.registry()));
        } else {
            println!("{}", codecs::render(base.registry(), args.verbose > 0));
        }
        return Ok(ExitCode::SUCCESS);
    }

    let started = std::time::Instant::now();
    let mut cfg = config::build(args, base)?;
    // Errors before the inputs are known go through a printer with the
    // defaults; the real one is built once the inputs are known.
    let printer = Printer::new(cfg.feedback, 0);

    let mut raw = cfg.inputs.clone();
    if let Some(list) = &cfg.files_from {
        let more = inputs::files_from(list, cfg.null)
            .map_err(|e| Failure::usage(format!("--files-from {}: {e}", list.display())))?;
        raw.extend(more);
    }
    let registry = cfg.sqzer.registry();
    let decodable = |p: &Path| {
        p.extension()
            .and_then(|e| Format::from_extension(&e.to_string_lossy()))
            .is_some_and(|f| registry.has_decoder(f))
    };
    let resolved = inputs::resolve(
        &raw,
        &inputs::Options {
            recursive: cfg.recursive,
            decodable: &decodable,
            include: &cfg.include,
            exclude: &cfg.exclude,
        },
    );
    for (arg, why) in &resolved.failures {
        printer.error(&format!("{arg}: {why}"));
    }
    if resolved.inputs.is_empty() {
        return Err(Failure::nothing("no input matched"));
    }
    cfg.finish(&resolved.inputs)?;
    let printer = Printer::new(cfg.feedback, resolved.inputs.len() as u64);

    let jobs = cfg.jobs.min(resolved.inputs.len()).max(1);
    let budget = PixelBudget::for_run(cfg.max_pixels, jobs);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|e| Failure::usage(format!("cannot start {jobs} jobs: {e}")))?;
    let ctx = job::Ctx {
        cfg: &cfg,
        budget: &budget,
        printer: &printer,
    };
    let inputs: &[Input] = &resolved.inputs;
    let tally = pool.install(|| {
        inputs
            .par_iter()
            .map(|input| job::process(input, &ctx))
            .reduce(Tally::default, Tally::merge)
    });
    printer.finish();
    printer.summary(&tally, started.elapsed());

    Ok(if tally.failed > 0 || !resolved.failures.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Colour for clap's own output, decided before parsing from a scan of
/// `--color` and `NO_COLOR`; the run's stderr colour is decided again
/// in `config` with the parsed value.
fn color_choice(argv: &[String]) -> ColorChoice {
    let mut when = When::Auto;
    let mut iter = argv.iter();
    while let Some(a) = iter.next() {
        let value = if a == "--color" {
            iter.next().map(String::as_str)
        } else {
            a.strip_prefix("--color=")
        };
        when = match value {
            Some("always") => When::Always,
            Some("never") => When::Never,
            _ => when,
        };
    }
    match when {
        When::Always => ColorChoice::Always,
        When::Never => ColorChoice::Never,
        When::Auto if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) => {
            ColorChoice::Never
        }
        When::Auto => ColorChoice::Auto,
    }
}
