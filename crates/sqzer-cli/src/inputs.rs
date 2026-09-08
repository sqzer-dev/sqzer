//! Turning positional arguments into files, ADR-0003 "Inputs and paths".
//!
//! The rule that ends the `rimage` path bugs: an argument that exists on
//! disk is a path and is never parsed as a glob. One that does not exist
//! and contains a glob metacharacter is expanded here, because Windows
//! shells do not. Anything else is an error, with a hint when the argument
//! is a `rimage` codec name.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use glob::{MatchOptions, Pattern};

/// One thing to process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// One image from stdin.
    Stdin,
    /// A file named directly, by path or glob.
    File(PathBuf),
    /// A file found under a directory argument; `root` is that argument,
    /// so `-o` can mirror the tree.
    InTree {
        /// The directory argument the file was found under.
        root: PathBuf,
        /// The file.
        path: PathBuf,
    },
}

impl Input {
    /// The file on disk, if any.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Stdin => None,
            Self::File(p) | Self::InTree { path: p, .. } => Some(p),
        }
    }

    /// The directory of the file relative to its tree root, for
    /// mirroring. Empty for a file named directly.
    pub fn relative_dir(&self) -> PathBuf {
        match self {
            Self::InTree { root, path } => path
                .parent()
                .and_then(|p| p.strip_prefix(root).ok())
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            Self::Stdin | Self::File(_) => PathBuf::new(),
        }
    }

    /// How the input is named in messages and JSON.
    pub fn display(&self) -> String {
        match self.path() {
            Some(p) => p.display().to_string(),
            None => "-".into(),
        }
    }
}

/// How the arguments resolved: the inputs to process and the arguments
/// that named nothing, each with its message.
#[derive(Debug, Default)]
pub struct Resolved {
    /// Inputs, in argument order, without duplicates.
    pub inputs: Vec<Input>,
    /// `(argument, why)` for every argument that resolved to nothing.
    pub failures: Vec<(String, String)>,
}

/// What the resolver needs to know.
pub struct Options<'a> {
    /// Descend into directories.
    pub recursive: bool,
    /// Whether a file found while walking a directory is worth
    /// decoding, by its extension.
    pub decodable: &'a dyn Fn(&Path) -> bool,
    /// `--include` globs. Empty means everything.
    pub include: &'a [Pattern],
    /// `--exclude` globs. Win over include.
    pub exclude: &'a [Pattern],
}

/// Resolve the positional arguments plus the lines of `--files-from`.
pub fn resolve(args: &[String], opts: &Options<'_>) -> Resolved {
    let mut out = Resolved::default();
    let mut seen = BTreeSet::new();
    for raw in args {
        let arg = strip_trailing_quote(raw);
        if arg == "-" {
            if seen.insert(PathBuf::from("-")) {
                out.inputs.push(Input::Stdin);
            }
            continue;
        }
        let path = Path::new(arg);
        match fs::metadata(path) {
            Ok(meta) => {
                if meta.is_dir() {
                    if !opts.recursive {
                        out.failures
                            .push((arg.to_string(), "is a directory; pass -r to recurse".into()));
                        continue;
                    }
                    let mut found = Vec::new();
                    walk(path, path, opts, &mut found);
                    if found.is_empty() {
                        out.failures
                            .push((arg.to_string(), "no images under it".into()));
                    }
                    for p in found {
                        if seen.insert(p.clone()) {
                            out.inputs.push(Input::InTree {
                                root: path.to_path_buf(),
                                path: p,
                            });
                        }
                    }
                } else if seen.insert(path.to_path_buf()) {
                    out.inputs.push(Input::File(path.to_path_buf()));
                }
            }
            Err(_) if has_glob_chars(arg) => {
                let mut any = false;
                let matches = glob::glob_with(arg, glob_options());
                let Ok(matches) = matches else {
                    out.failures
                        .push((arg.to_string(), "is not a valid glob".into()));
                    continue;
                };
                for entry in matches.flatten() {
                    any = true;
                    if entry.is_dir() {
                        if opts.recursive {
                            let mut found = Vec::new();
                            walk(&entry, &entry, opts, &mut found);
                            for p in found {
                                if seen.insert(p.clone()) {
                                    out.inputs.push(Input::InTree {
                                        root: entry.clone(),
                                        path: p,
                                    });
                                }
                            }
                        }
                    } else if seen.insert(entry.clone()) {
                        out.inputs.push(Input::File(entry));
                    }
                }
                if !any {
                    out.failures
                        .push((arg.to_string(), "no files match".into()));
                }
            }
            Err(e) => {
                let why = match e.kind() {
                    std::io::ErrorKind::NotFound => "no such file".to_string(),
                    _ => e.to_string(),
                };
                out.failures.push((arg.to_string(), why));
            }
        }
    }
    out
}

/// Read the paths of `--files-from`: one per line, or NUL separated with
/// `-0`. `-` reads stdin.
pub fn files_from(file: &Path, null: bool) -> std::io::Result<Vec<String>> {
    let text = if file == Path::new("-") {
        let mut s = String::new();
        std::io::stdin().lock().read_to_string(&mut s)?;
        s
    } else {
        fs::read_to_string(file)?
    };
    let sep = if null { '\0' } else { '\n' };
    Ok(text
        .split(sep)
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// `cmd.exe` turns `"C:\dir\"` into `C:\dir"`; one trailing quote is a
/// quoting artefact, not a file name.
pub fn strip_trailing_quote(arg: &str) -> &str {
    arg.strip_suffix('"').unwrap_or(arg)
}

fn has_glob_chars(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Case-insensitive so `*.png` finds `.PNG` (rimage #93); `*` crosses
/// directory separators, as it does for `--include`.
fn glob_options() -> MatchOptions {
    MatchOptions {
        case_sensitive: false,
        require_literal_separator: false,
        require_literal_leading_dot: true,
    }
}

/// Files under `dir`, sorted, filtered by extension and the include and
/// exclude globs matched against the path relative to `root`.
fn walk(root: &Path, dir: &Path, opts: &Options<'_>, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(root, &path, opts, out);
            continue;
        }
        if !(opts.decodable)(&path) {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let rel = rel.to_string_lossy().replace('\\', "/");
        let matches = |p: &Pattern| p.matches_with(&rel, glob_options());
        if opts.exclude.iter().any(matches) {
            continue;
        }
        if !opts.include.is_empty() && !opts.include.iter().any(matches) {
            continue;
        }
        out.push(path);
    }
}

/// The `rimage` codec names and the `-f` value each maps to. A run whose
/// first positional is one of these, and is not a file, gets a rewrite
/// hint instead of "no such file".
pub const RIMAGE_CODECS: &[(&str, &str)] = &[
    ("mozjpeg", "jpeg"),
    ("oxipng", "png"),
    ("webp", "webp"),
    ("avif", "avif"),
    ("jxl", "jxl"),
    ("png", "png"),
    ("jpeg", "jpeg"),
];

/// The hint for a `rimage`-shaped command line, or `None` when the
/// arguments do not look like one. `argv` excludes the program name.
pub fn rimage_hint(argv: &[String]) -> Option<String> {
    let first = argv.first()?;
    let (_, format) = RIMAGE_CODECS.iter().find(|(name, _)| name == first)?;
    if Path::new(first).exists() {
        return None;
    }
    let rest: Vec<String> = argv[1..]
        .iter()
        .map(|a| match a.as_str() {
            "-d" => "-o".to_string(),
            "-s" => "--suffix".to_string(),
            "-t" => "-j".to_string(),
            "--quantization" => "--codec-opt png:colors=".to_string(),
            other => other.to_string(),
        })
        .collect();
    let mut line = format!("sqzer -f {format}");
    if !rest.is_empty() {
        line.push(' ');
        line.push_str(&rest.join(" "));
    }
    Some(format!(
        "`sqzer {first}` is rimage syntax. try:\n    {line}\nsee the README for the flag mapping"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sqzer-inputs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    fn opts<'a>(recursive: bool, include: &'a [Pattern], exclude: &'a [Pattern]) -> Options<'a> {
        Options {
            recursive,
            decodable: &|p: &Path| {
                p.extension()
                    .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("png"))
            },
            include,
            exclude,
        }
    }

    fn names(r: &Resolved) -> Vec<String> {
        r.inputs
            .iter()
            .map(|i| {
                i.path()
                    .and_then(Path::file_name)
                    .map_or("-".into(), |n| n.to_string_lossy().into_owned())
            })
            .collect()
    }

    #[test]
    fn existing_path_is_literal_even_with_glob_chars() {
        let dir = tmp("literal");
        let odd = dir.join("a[1].png");
        touch(&odd);
        touch(&dir.join("a1.png"));
        let r = resolve(
            &[odd.to_string_lossy().into_owned()],
            &opts(false, &[], &[]),
        );
        assert_eq!(names(&r), vec!["a[1].png"]);
        assert!(r.failures.is_empty());
    }

    #[test]
    fn missing_path_with_glob_chars_expands_case_insensitively() {
        let dir = tmp("glob");
        touch(&dir.join("UPPER.PNG"));
        touch(&dir.join("lower.png"));
        touch(&dir.join("skip.jpg"));
        let pat = dir.join("*.png").to_string_lossy().into_owned();
        let r = resolve(&[pat], &opts(false, &[], &[]));
        let mut got = names(&r);
        got.sort();
        assert_eq!(got, vec!["UPPER.PNG", "lower.png"]);
        let r = resolve(
            &[dir.join("none*.png").to_string_lossy().into_owned()],
            &opts(false, &[], &[]),
        );
        assert!(r.inputs.is_empty());
        assert_eq!(r.failures[0].1, "no files match");
    }

    #[test]
    fn missing_plain_path_is_a_failure_and_a_directory_needs_recursive() {
        let dir = tmp("missing");
        let r = resolve(
            &[dir.join("nope.png").to_string_lossy().into_owned()],
            &opts(false, &[], &[]),
        );
        assert_eq!(r.failures[0].1, "no such file");
        let r = resolve(
            &[dir.to_string_lossy().into_owned()],
            &opts(false, &[], &[]),
        );
        assert!(r.failures[0].1.contains("pass -r"));
    }

    #[test]
    fn walk_mirrors_and_filters() {
        let dir = tmp("walk");
        touch(&dir.join("a/b/c.png"));
        touch(&dir.join("a/d.PNG"));
        touch(&dir.join("e.png"));
        touch(&dir.join("f.txt"));
        touch(&dir.join("a/b/skip.png"));
        let exclude = [Pattern::new("**/skip*").unwrap()];
        let r = resolve(
            &[dir.to_string_lossy().into_owned()],
            &opts(true, &[], &exclude),
        );
        assert_eq!(names(&r), vec!["c.png", "d.PNG", "e.png"]);
        let rel: Vec<PathBuf> = r.inputs.iter().map(Input::relative_dir).collect();
        assert_eq!(
            rel,
            vec![PathBuf::from("a/b"), PathBuf::from("a"), PathBuf::new()]
        );
        let include = [Pattern::new("a/*").unwrap()];
        let r = resolve(
            &[dir.to_string_lossy().into_owned()],
            &opts(true, &include, &[]),
        );
        assert_eq!(names(&r), vec!["c.png", "skip.png", "d.PNG"]);
    }

    #[test]
    fn duplicates_collapse_and_stdin_is_one_input() {
        let dir = tmp("dup");
        let f = dir.join("x.png");
        touch(&f);
        let s = f.to_string_lossy().into_owned();
        let r = resolve(
            &[s.clone(), s, "-".into(), "-".into()],
            &opts(false, &[], &[]),
        );
        assert_eq!(r.inputs.len(), 2);
        assert_eq!(r.inputs[1], Input::Stdin);
        assert_eq!(r.inputs[1].display(), "-");
    }

    #[test]
    fn trailing_quote_is_stripped() {
        assert_eq!(strip_trailing_quote(r#"C:\dir""#), r"C:\dir");
        assert_eq!(strip_trailing_quote("plain"), "plain");
        let dir = tmp("quote");
        let f = dir.join("q.png");
        touch(&f);
        let quoted = format!("{}\"", f.to_string_lossy());
        let r = resolve(&[quoted], &opts(false, &[], &[]));
        assert_eq!(names(&r), vec!["q.png"]);
    }

    #[test]
    fn files_from_splits_on_newline_or_nul() {
        let dir = tmp("from");
        let list = dir.join("list");
        fs::write(&list, "a.png\r\nb.png\n\n").unwrap();
        assert_eq!(files_from(&list, false).unwrap(), vec!["a.png", "b.png"]);
        fs::write(&list, "a b.png\0c\n.png\0").unwrap();
        assert_eq!(files_from(&list, true).unwrap(), vec!["a b.png", "c\n.png"]);
    }

    #[test]
    fn rimage_syntax_gets_a_rewrite() {
        let argv: Vec<String> = ["mozjpeg", "-q", "75", "-d", "out", "in.jpg"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let hint = rimage_hint(&argv).unwrap();
        assert!(hint.contains("sqzer -f jpeg -q 75 -o out in.jpg"), "{hint}");
        assert!(rimage_hint(&["photo.jpg".to_string()]).is_none());
        assert!(rimage_hint(&[]).is_none());
    }
}
