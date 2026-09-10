//! Where an output goes, ADR-0003 "Output placement".
//!
//! The default is a sibling of the input with the same stem and the new
//! extension. `-o` names a file or a directory, `--suffix` and
//! `--template` change the name, `--in-place` is the only way an output
//! path may equal an input path, and `-r` with `-o` mirrors each input
//! tree under the output directory.

use std::path::{Path, PathBuf};

use sqzer::core::codec::Format;
use sqzer::core::params::Resolved;

use crate::inputs::Input;

/// The placement flags, checked.
#[derive(Debug, Clone, Default)]
pub struct Placement {
    /// `-o`.
    pub output: Option<PathBuf>,
    /// `-o` names a file: one input, one format, and a path with an
    /// extension.
    pub single_file: bool,
    /// `--suffix`.
    pub suffix: Option<String>,
    /// `--template`.
    pub template: Option<Template>,
    /// `--in-place`.
    pub in_place: bool,
}

/// What a name is built from.
#[derive(Debug, Clone, Copy)]
pub struct Naming<'a> {
    /// The input.
    pub input: &'a Input,
    /// What the input was detected as.
    pub input_format: Format,
    /// The output format.
    pub format: Format,
    /// Output width.
    pub width: u32,
    /// Output height.
    pub height: u32,
    /// The quality the encoder ran with, once known. `None` in a dry run
    /// with a pending search.
    pub quality: Option<Resolved>,
}

/// Why an output cannot go where the flags say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceError {
    /// The resolved output is the input, and `--in-place` was not given.
    OverwritesInput,
    /// `--in-place` with a format other than the input's.
    InPlaceChangesFormat {
        /// Input format.
        from: Format,
        /// Requested format.
        to: Format,
    },
}

impl std::fmt::Display for PlaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OverwritesInput => {
                write!(
                    f,
                    "output would overwrite input; use -o, --suffix or --in-place"
                )
            }
            Self::InPlaceChangesFormat { from, to } => write!(
                f,
                "--in-place would turn {from} into {to}; drop -f or use -o"
            ),
        }
    }
}

impl Placement {
    /// The path for one output. Stdin input is handled by the caller;
    /// this takes a file.
    ///
    /// # Errors
    /// [`PlaceError`] when the path would clobber the input.
    pub fn resolve(&self, n: &Naming<'_>) -> Result<PathBuf, PlaceError> {
        let input = n
            .input
            .path()
            .expect("stdin does not go through the placement resolver");
        if self.in_place {
            if n.format != n.input_format {
                return Err(PlaceError::InPlaceChangesFormat {
                    from: n.input_format,
                    to: n.format,
                });
            }
            return Ok(input.to_path_buf());
        }
        let out = if self.single_file {
            self.output.clone().expect("single_file implies -o")
        } else {
            let dir = match &self.output {
                Some(dir) => dir.join(n.input.relative_dir()),
                None => input.parent().map(Path::to_path_buf).unwrap_or_default(),
            };
            dir.join(self.file_name(n, input))
        };
        if same_path(&out, input) {
            return Err(PlaceError::OverwritesInput);
        }
        Ok(out)
    }

    fn file_name(&self, n: &Naming<'_>, input: &Path) -> PathBuf {
        let stem = stem_of(input);
        if let Some(t) = &self.template {
            return PathBuf::from(t.render(n, input, &stem));
        }
        let suffix = self.suffix.as_deref().unwrap_or("");
        PathBuf::from(format!("{stem}{suffix}.{}", n.format.extension()))
    }
}

/// Everything before the last dot of the file name (rimage #266), or the
/// whole name when there is no dot.
pub fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether two paths name the same file, without either having to exist.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| -> String {
        let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let mut parts = Vec::new();
        for c in abs.components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    parts.pop();
                }
                other => parts.push(other.as_os_str().to_string_lossy().into_owned()),
            }
        }
        let joined = parts.join("/");
        if cfg!(windows) {
            joined.to_lowercase()
        } else {
            joined
        }
    };
    norm(a) == norm(b)
}

/// The placeholders `--template` accepts.
pub const PLACEHOLDERS: &[&str] = &[
    "stem", "ext", "width", "height", "format", "quality", "dir", "name",
];

/// A checked `--template`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template(String);

impl Template {
    /// Check every `{placeholder}` is known and the braces balance.
    ///
    /// # Errors
    /// A message naming the offending placeholder.
    pub fn new(t: &str) -> Result<Self, String> {
        let mut rest = t;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                return Err(format!("unclosed `{{` in template `{t}`"));
            };
            let key = &after[..close];
            if !PLACEHOLDERS.contains(&key) {
                return Err(format!(
                    "unknown placeholder `{{{key}}}` in template; one of {}",
                    PLACEHOLDERS
                        .iter()
                        .map(|p| format!("{{{p}}}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            rest = &after[close + 1..];
        }
        if rest.contains('}') {
            return Err(format!("stray `}}` in template `{t}`"));
        }
        Ok(Self(t.to_string()))
    }

    fn render(&self, n: &Naming<'_>, input: &Path, stem: &str) -> String {
        let quality = match n.quality {
            Some(Resolved::Quality(q)) => format!("{}", q.round()),
            Some(Resolved::Lossless) => "lossless".to_string(),
            None => "auto".to_string(),
        };
        let dir = input
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = input
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.0
            .replace("{stem}", stem)
            .replace("{ext}", n.format.extension())
            .replace("{width}", &n.width.to_string())
            .replace("{height}", &n.height.to_string())
            .replace("{format}", crate::cli::format_name(n.format))
            .replace("{quality}", &quality)
            .replace("{dir}", &dir)
            .replace("{name}", &name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naming(input: &Input, format: Format) -> Naming<'_> {
        Naming {
            input,
            input_format: Format::Jpeg,
            format,
            width: 1600,
            height: 900,
            quality: Some(Resolved::Quality(72.0)),
        }
    }

    #[test]
    fn default_is_a_sibling_with_the_new_extension() {
        let input = Input::File("photos/2024/trip.holiday.jpg".into());
        let out = Placement::default()
            .resolve(&naming(&input, Format::Avif))
            .unwrap();
        assert_eq!(out, PathBuf::from("photos/2024/trip.holiday.avif"));
        // Same format, no flags: refused.
        let err = Placement::default()
            .resolve(&naming(&input, Format::Jpeg))
            .unwrap_err();
        assert_eq!(err, PlaceError::OverwritesInput);
    }

    #[test]
    fn suffix_template_and_directory() {
        let input = Input::File("in/a.jpg".into());
        let p = Placement {
            suffix: Some("-min".into()),
            ..Default::default()
        };
        assert_eq!(
            p.resolve(&naming(&input, Format::Jpeg)).unwrap(),
            PathBuf::from("in/a-min.jpg")
        );
        let p = Placement {
            template: Some(Template::new("{stem}-{width}w-q{quality}.{ext}").unwrap()),
            output: Some("out".into()),
            ..Default::default()
        };
        assert_eq!(
            p.resolve(&naming(&input, Format::WebP)).unwrap(),
            PathBuf::from("out/a-1600w-q72.webp")
        );
        let p = Placement {
            template: Some(Template::new("{dir}/{name}.{format}").unwrap()),
            ..Default::default()
        };
        let mut n = naming(&input, Format::Avif);
        n.quality = None;
        assert_eq!(p.resolve(&n).unwrap(), PathBuf::from("in/in/a.jpg.avif"));
    }

    #[test]
    fn single_file_output_and_tree_mirroring() {
        let input = Input::File("a.jpg".into());
        let p = Placement {
            output: Some("out/b.avif".into()),
            single_file: true,
            ..Default::default()
        };
        assert_eq!(
            p.resolve(&naming(&input, Format::Avif)).unwrap(),
            PathBuf::from("out/b.avif")
        );
        let input = Input::InTree {
            root: "assets".into(),
            path: "assets/img/deep/x.jpg".into(),
        };
        let p = Placement {
            output: Some("dist".into()),
            ..Default::default()
        };
        assert_eq!(
            p.resolve(&naming(&input, Format::Avif)).unwrap(),
            PathBuf::from("dist/img/deep/x.avif")
        );
        // Without -o the mirror is the input's own directory.
        assert_eq!(
            Placement::default()
                .resolve(&naming(&input, Format::Avif))
                .unwrap(),
            PathBuf::from("assets/img/deep/x.avif")
        );
    }

    #[test]
    fn in_place_keeps_the_path_and_the_format() {
        let input = Input::File("a.jpg".into());
        let p = Placement {
            in_place: true,
            ..Default::default()
        };
        assert_eq!(
            p.resolve(&naming(&input, Format::Jpeg)).unwrap(),
            PathBuf::from("a.jpg")
        );
        assert!(matches!(
            p.resolve(&naming(&input, Format::Avif)),
            Err(PlaceError::InPlaceChangesFormat { .. })
        ));
    }

    /// The ADR-0003 property: over every combination of the placement
    /// flags, the resolved output equals the input only under --in-place.
    #[test]
    fn output_equals_input_only_in_place() {
        let inputs = [
            Input::File("a.jpg".into()),
            Input::File("./a.jpg".into()),
            Input::File("dir/a.b.jpg".into()),
            Input::InTree {
                root: "dir".into(),
                path: "dir/sub/a.jpg".into(),
            },
        ];
        let suffixes = [None, Some(String::new()), Some("-min".to_string())];
        let templates = [
            None,
            Some("{stem}.{ext}"),
            Some("{name}"),
            Some("{dir}/{name}"),
            Some("{stem}-{width}.{ext}"),
        ];
        let outputs: [Option<PathBuf>; 3] = [None, Some("dir".into()), Some("dir/sub".into())];
        let mut checked = 0;
        for input in &inputs {
            for suffix in &suffixes {
                for template in templates {
                    for output in &outputs {
                        for in_place in [false, true] {
                            for format in [Format::Jpeg, Format::Avif] {
                                let p = Placement {
                                    output: output.clone(),
                                    single_file: false,
                                    suffix: suffix.clone(),
                                    template: template.map(|t| Template::new(t).unwrap()),
                                    in_place,
                                };
                                let n = naming(input, format);
                                if let Ok(out) = p.resolve(&n) {
                                    checked += 1;
                                    let equal = same_path(&out, input.path().unwrap());
                                    assert!(
                                        !equal || in_place,
                                        "{input:?} {p:?} -> {}",
                                        out.display()
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 100);
    }

    #[test]
    fn templates_are_checked() {
        assert!(Template::new("{stem}.{ext}").is_ok());
        assert!(Template::new("{nope}").unwrap_err().contains("{nope}"));
        assert!(Template::new("{stem").is_err());
        assert!(Template::new("stem}").is_err());
    }

    #[test]
    fn stems_split_at_the_last_dot() {
        assert_eq!(stem_of(Path::new("a.b.c.PNG")), "a.b.c");
        assert_eq!(stem_of(Path::new("noext")), "noext");
        assert_eq!(stem_of(Path::new("dir/.hidden")), ".hidden");
    }

    #[test]
    fn same_path_normalises() {
        assert!(same_path(Path::new("./a/b.jpg"), Path::new("a/b.jpg")));
        assert!(same_path(Path::new("a/../a/b.jpg"), Path::new("a/b.jpg")));
        assert!(!same_path(Path::new("a/b.jpg"), Path::new("a/b.avif")));
    }
}
