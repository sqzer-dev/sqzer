//! End-to-end checks of the `sqzer` binary: the six command shapes of
//! ADR-0001 D5, the four exit codes, the path corpus of ADR-0003 and the
//! feedback channels. Every test runs in its own scratch directory under
//! the system temp dir and copies fixtures in, so nothing touches
//! `tests/fixtures`.

#![cfg(feature = "portable")]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

/// Which backends a `native` build carries on this target.
#[path = "../src/native_set.rs"]
mod native_set;

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("sqzer-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    /// Copy a fixture in under `as_name`, creating parent directories.
    fn fixture(&self, name: &str, as_name: &str) -> PathBuf {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name);
        let dst = self.path(as_name);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(&src, &dst).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
        dst
    }

    fn sqzer(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sqzer"));
        // stderr is not a terminal here, so progress and colour are off
        // unless a test asks for them.
        cmd.current_dir(&self.dir)
            .env_remove("NO_COLOR")
            .args(["-j", "1"]);
        cmd
    }
}

fn run(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_with_stdin(cmd: &mut Command, input: &[u8]) -> (i32, Vec<u8>, String) {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json_lines(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON: {l}: {e}")))
        .collect()
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG")
}

fn is_avif(bytes: &[u8]) -> bool {
    bytes.len() > 12 && &bytes[4..8] == b"ftyp"
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

// ---- The six shapes

#[test]
fn shape_1_one_photo_becomes_a_sibling_avif() {
    let sb = Sandbox::new("shape1");
    sb.fixture("pattern-rgb.jpg", "photo.jpg");
    let (code, out, err) = run(sb.sqzer().args(["photo.jpg", "--json"]));
    assert_eq!(code, 0, "{err}");
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 1);
    let r = &lines[0];
    assert_eq!(r["status"], "written");
    assert_eq!(r["input"], "photo.jpg");
    assert_eq!(r["output"], "photo.avif");
    assert_eq!(r["format"], "avif");
    assert_eq!(r["input_format"], "jpeg");
    // A native build's libavif takes AVIF over from ravif.
    let (backend, tier) = if cfg!(feature = "native") {
        ("libavif", "native")
    } else {
        ("ravif", "portable")
    };
    assert_eq!(r["backend"], backend);
    assert_eq!(r["tier"], tier);
    assert_eq!(r["content"], "photo");
    assert_eq!(r["target"], 70.0);
    assert!(r["score"].is_number(), "{r}");
    assert!(r["quality"].is_number(), "{r}");
    assert!(r["iterations"].as_u64().unwrap() <= 6, "{r}");
    assert!(r["trials"].is_array(), "{r}");
    assert!(is_avif(&fs::read(sb.path("photo.avif")).unwrap()));
}

#[test]
fn shape_2_one_input_several_formats() {
    let sb = Sandbox::new("shape2");
    sb.fixture("pattern-rgb.jpg", "photo.jpg");
    let (code, out, err) = run(sb.sqzer().args([
        "photo.jpg",
        "-f",
        "png,webp",
        "--lossless",
        "-o",
        "out",
        "--force",
        "--json",
    ]));
    assert_eq!(code, 0, "{err}");
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|l| l["status"] == "written" && l["lossless"] == true)
    );
    assert!(is_png(&fs::read(sb.path("out/photo.png")).unwrap()));
    assert_eq!(
        &fs::read(sb.path("out/photo.webp")).unwrap()[8..12],
        b"WEBP"
    );
}

#[test]
fn shape_3_recursive_mirrors_the_tree() {
    let sb = Sandbox::new("shape3");
    sb.fixture("pattern-rgb.webp", "assets/img/deep/a.webp");
    sb.fixture("pattern-rgba.webp", "assets/b.webp");
    sb.fixture("pattern-rgb.webp", "assets/notes.txt");
    let (code, out, err) = run(sb.sqzer().args([
        "assets",
        "-r",
        "-f",
        "png",
        "--lossless",
        "-o",
        "dist",
        "--force",
        "--json",
    ]));
    assert_eq!(code, 0, "{err}");
    let mut outputs: Vec<String> = json_lines(&out)
        .iter()
        .map(|l| l["output"].as_str().unwrap().replace('\\', "/"))
        .collect();
    outputs.sort();
    assert_eq!(outputs, vec!["dist/b.png", "dist/img/deep/a.png"]);
    assert!(sb.path("dist/img/deep/a.png").exists());
    assert!(sb.path("dist/b.png").exists());
    // A directory without -r is refused, and alone it means nothing matched.
    let (code, _, err) = run(sb.sqzer().args(["assets"]));
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("pass -r"), "{err}");
    assert!(err.contains("no input matched"), "{err}");
}

#[test]
fn shape_4_lossless_preset_conversion() {
    let sb = Sandbox::new("shape4");
    sb.fixture("pattern-rgb.webp", "a.webp");
    sb.fixture("pattern-rgba.webp", "b.webp");
    let (code, out, err) = run(sb.sqzer().args([
        "a.webp", "b.webp", "--preset", "lossless", "-f", "png", "--force", "--json",
    ]));
    assert_eq!(code, 0, "{err}");
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|l| l["lossless"] == true && l["status"] == "written")
    );
    assert!(is_png(&fs::read(sb.path("a.png")).unwrap()));
    assert!(is_png(&fs::read(sb.path("b.png")).unwrap()));
}

#[test]
fn shape_5_explicit_target_is_searched_for() {
    let sb = Sandbox::new("shape5");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    let (code, out, err) = run(sb.sqzer().args([
        "in.jpg", "--target", "60", "-f", "avif", "--json", "--force",
    ]));
    assert_eq!(code, 0, "{err}");
    let r = &json_lines(&out)[0];
    assert_eq!(r["target"], 60.0);
    assert!(r["reached"].is_boolean(), "{r}");
    assert!(r["score"].as_f64().unwrap() > 50.0, "{r}");
}

/// Width and height from a PNG's `IHDR`.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert!(is_png(bytes));
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn max_width_resizes_once_for_every_output() {
    let sb = Sandbox::new("resize");
    // Stored 32 x 48 with EXIF orientation 6: the bound is on the 48 wide
    // picture as displayed.
    sb.fixture("pattern-rot90.jpg", "in.jpg");
    let (code, out, err) = run(sb.sqzer().args([
        "in.jpg",
        "-t",
        "60",
        "--max-width",
        "24",
        "-f",
        "png,jpeg",
        "--template",
        "{stem}-{width}w.{ext}",
        "--force",
        "--json",
    ]));
    assert_eq!(code, 0, "{err}");
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 2, "{out}");
    for r in &lines {
        assert_eq!(r["status"], "written", "{r}");
        assert_eq!((&r["width"], &r["height"]), (&48.into(), &32.into()), "{r}");
        assert_eq!(r["output_width"], 24, "{r}");
        assert_eq!(r["output_height"], 16, "{r}");
    }
    assert_eq!(lines[0]["output"], "in-24w.png");
    assert_eq!(lines[1]["output"], "in-24w.jpg");
    // The search ran against the resized image and says so.
    assert!(lines[1]["score"].is_number(), "{}", lines[1]);
    assert_eq!(
        png_size(&fs::read(sb.path("in-24w.png")).unwrap()),
        (24, 16)
    );
    assert!(is_jpeg(&fs::read(sb.path("in-24w.jpg")).unwrap()));
}

#[test]
fn resize_never_enlarges_and_the_thumbnail_preset_uses_it() {
    let sb = Sandbox::new("resize-bounds");
    sb.fixture("pattern-rgba.webp", "in.webp");
    let size = |args: &[&str]| {
        let (code, out, err) = run(sb
            .sqzer()
            .args(["in.webp", "-f", "png", "--force", "--json"])
            .args(args));
        assert_eq!(code, 0, "{args:?}: {err}");
        let r = &json_lines(&out)[0];
        assert_eq!(r["alpha"], true);
        let reported = (
            u32::try_from(r["output_width"].as_u64().unwrap()).unwrap(),
            u32::try_from(r["output_height"].as_u64().unwrap()).unwrap(),
        );
        assert_eq!(png_size(&fs::read(sb.path("in.png")).unwrap()), reported);
        reported
    };
    assert_eq!(size(&[]), (48, 32));
    assert_eq!(size(&["--max-width", "1600"]), (48, 32));
    assert_eq!(
        size(&["--max-width", "1600", "--max-height", "16"]),
        (24, 16)
    );
    // 48 x 32 is inside the preset's 512 x 512 box.
    assert_eq!(size(&["--preset", "thumbnail"]), (48, 32));
    assert_eq!(
        size(&["--preset", "thumbnail", "--max-height", "8"]),
        (12, 8)
    );
    // A dry run names the size it would write.
    let (code, _, err) =
        run(sb
            .sqzer()
            .args(["in.webp", "-n", "--max-width", "24", "--progress", "always"]));
    assert_eq!(code, 0);
    assert!(err.contains("48x32 -> 24x16 alpha"), "{err}");
}

#[test]
fn shape_6_json_is_the_only_thing_on_stdout() {
    let sb = Sandbox::new("shape6");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    fs::write(sb.path("garbage.png"), b"not an image at all").unwrap();
    let (code, out, _) = run(sb.sqzer().args([
        "in.jpg",
        "garbage.png",
        "-f",
        "jpeg",
        "-q",
        "50",
        "--suffix",
        "-min",
        "--force",
        "--json",
        "-v",
        "--progress",
        "always",
    ]));
    assert_eq!(code, 1);
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 2, "every stdout line is JSON: {out}");
    assert_eq!(lines[0]["status"], "written");
    assert_eq!(lines[0]["quality"], 50.0);
    assert!(lines[0]["ratio"].is_number());
    assert_eq!(lines[1]["status"], "failed");
    assert!(lines[1]["error"].as_str().unwrap().contains("unrecognised"));
}

#[test]
fn shape_7_list_codecs() {
    let sb = Sandbox::new("shape7");
    let (code, out, _) = run(sb.sqzer().arg("--list-codecs"));
    assert_eq!(code, 0);
    assert!(out.contains("jxl-oxide"), "{out}");
    assert!(!out.contains("jpeg:progressive"), "{out}");
    // The native tier owns WebP and AVIF on every target; JPEG, JPEG XL
    // and HEIC depend on what the target carries, see `native_set`.
    if cfg!(feature = "native") {
        assert!(
            out.contains("tiers in this build: native, portable"),
            "{out}"
        );
        assert!(out.contains("webpx (native), lossy and lossless"), "{out}");
    } else {
        assert!(out.contains("tiers in this build: portable"), "{out}");
    }
    let jpeg = if native_set::JPEGLI {
        "jpegli (native), lossy"
    } else {
        "mozjpeg-rs (portable), lossy"
    };
    assert!(out.contains(jpeg), "{out}");
    let jxl = if native_set::JXL {
        "gamut-jxl (native), lossy and lossless"
    } else {
        "none; needs `native-jxl`"
    };
    assert!(out.contains(jxl), "{out}");
    let heic = if !native_set::HEIC {
        "none; needs `native-heif`"
    } else if cfg!(target_os = "macos") {
        "imageio (native"
    } else if cfg!(windows) {
        "wic (native"
    } else {
        "libheif (native"
    };
    assert!(out.contains(heic), "{out}");
    let (code, out, _) = run(sb.sqzer().args(["--list-codecs", "-v"]));
    assert_eq!(code, 0);
    if cfg!(feature = "native") {
        assert!(out.contains("webp:sharp_yuv=false"), "{out}");
    } else {
        assert!(out.contains("avif:bit_depth=auto"), "{out}");
    }
    if native_set::JXL {
        assert!(out.contains("jxl:container=false"), "{out}");
    }
    if !native_set::JPEGLI {
        assert!(out.contains("jpeg:progressive=true"), "{out}");
    }
    let (code, out, _) = run(sb.sqzer().args(["--list-codecs", "--json"]));
    assert_eq!(code, 0);
    let lines = json_lines(&out);
    let jpeg = lines.iter().find(|l| l["format"] == "jpeg").unwrap();
    let jxl = lines.iter().find(|l| l["format"] == "jxl").unwrap();
    if native_set::JPEGLI {
        assert_eq!(jpeg["encoder"]["backend"], "jpegli");
    } else {
        assert_eq!(jpeg["encoder"]["backend"], "mozjpeg-rs");
        assert_eq!(jpeg["encoder"]["options"][0]["key"], "jpeg:progressive");
    }
    if native_set::JXL {
        assert_eq!(jxl["encoder"]["backend"], "gamut-jxl");
        assert_eq!(jxl["encoder"]["tier"], "native");
    } else {
        assert!(jxl.get("encoder").is_none());
        assert_eq!(jxl["encoder_features"][0], "native-jxl");
    }
}

// ---- Exit codes, one batch test each

#[test]
fn exit_0_includes_skipped_as_larger() {
    let sb = Sandbox::new("exit0");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    let (code, out, err) = run(sb
        .sqzer()
        .args(["in.jpg", "-f", "png", "--lossless", "--json"]));
    assert_eq!(code, 0, "{err}");
    let r = &json_lines(&out)[0];
    assert_eq!(
        r["status"], "skipped",
        "a lossless PNG of a 673 byte JPEG is larger: {r}"
    );
    assert!(r["reason"].as_str().unwrap().contains("--force"));
    assert!(!sb.path("in.png").exists());
    let (code, out, _) =
        run(sb
            .sqzer()
            .args(["in.jpg", "-f", "png", "--lossless", "--json", "--force"]));
    assert_eq!(code, 0);
    assert_eq!(json_lines(&out)[0]["status"], "written");
    assert!(sb.path("in.png").exists());
}

#[test]
fn exit_1_when_one_input_of_a_batch_fails() {
    let sb = Sandbox::new("exit1");
    sb.fixture("pattern-rgb.webp", "ok.webp");
    fs::write(sb.path("bad.webp"), b"RIFF____WEBPVP8 broken").unwrap();
    let (code, out, err) = run(sb.sqzer().args([
        "ok.webp",
        "bad.webp",
        "missing.webp",
        "-f",
        "png",
        "--lossless",
        "--force",
        "--json",
    ]));
    assert_eq!(code, 1);
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["status"], "written");
    assert_eq!(lines[1]["status"], "failed");
    assert!(err.contains("bad.webp"), "{err}");
    assert!(err.contains("missing.webp: no such file"), "{err}");
    assert!(sb.path("ok.png").exists());
}

#[test]
fn exit_2_on_argument_errors() {
    let sb = Sandbox::new("exit2");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    for (args, needle) in [
        (
            &["in.jpg", "--target", "70", "--quality", "80"][..],
            "cannot be used with",
        ),
        (
            &["in.jpg", "-x", "jpeg:nope=1"],
            "unknown jpeg option `nope`",
        ),
        (&["in.jpg", "-x", "avif:bit_depth"], "codec:key=value"),
        (&["-", "-o", "out"], "exactly one -f"),
        (&["-", "-f", "png", "--json"], "both go to stdout"),
        (&["in.jpg", "--template", "{nope}"], "unknown placeholder"),
        (&["in.jpg", "--backup"], "in-place"),
        (&["in.jpg", "-f", "gif"], "input format only"),
        (&["in.jpg", "--include", "[", "-r"], "not a valid glob"),
        (&[], "no input given"),
    ] {
        let (code, _, err) = run(sb.sqzer().args(args));
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains(needle), "{args:?}: {err}");
    }
    assert!(!sb.path("in.avif").exists());
}

/// A HEIC is recognised by every build. Without `native-heif` the error
/// names the feature; with it, the file decodes or the error names what
/// this machine is missing. Neither is "unrecognised".
#[test]
fn heic_is_never_unrecognised() {
    let sb = Sandbox::new("heic");
    sb.fixture("pattern-rgb.heic", "photo.heic");
    let (code, out, err) = run(sb.sqzer().args(["photo.heic", "-f", "png", "--json"]));
    assert!(!err.contains("unrecognised"), "{err}");
    let lines = json_lines(&out);
    if native_set::HEIC {
        // The decode either works or reports the missing library; a
        // failed file is exit 1, like any other per-file failure.
        if code == 0 {
            assert_eq!(lines[0]["format"], "png", "{out}");
        } else {
            assert_eq!(code, 1, "{err}");
            assert_eq!(lines[0]["status"], "failed", "{out}");
            let error = lines[0]["error"].as_str().unwrap();
            assert!(error.starts_with("no usable HEIC decoder"), "{error}");
        }
    } else {
        assert_eq!(code, 1, "{err}");
        assert_eq!(lines[0]["status"], "failed", "{out}");
        let error = lines[0]["error"].as_str().unwrap();
        assert!(
            error.contains("no decoder for HEIC in this build"),
            "{error}"
        );
        assert!(error.contains("native-heif"), "{error}");
    }
}

#[test]
fn exit_3_when_nothing_can_be_done() {
    let sb = Sandbox::new("exit3");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    let (code, _, err) = run(sb.sqzer().args(["nope*.png", "also-missing.jpg"]));
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("nope*.png: no files match"), "{err}");
    assert!(err.contains("no input matched"), "{err}");

    // A native build writes JPEG XL and lossy WebP; the rest of this test
    // is about the portable build's honesty.
    if cfg!(feature = "native") {
        return;
    }
    let (code, _, err) = run(sb.sqzer().args(["in.jpg", "-f", "jxl"]));
    assert_eq!(code, 3, "{err}");
    assert!(
        err.starts_with("error: no JPEG XL encoder in this build"),
        "{err}"
    );
    assert!(
        err.contains("this build encodes: JPEG (mozjpeg-rs, portable)"),
        "{err}"
    );
    assert!(err.contains("`native-jxl`"), "{err}");
    assert!(err.contains("sqzer --list-codecs"), "{err}");

    let (code, _, err) = run(sb.sqzer().args(["in.jpg", "-f", "webp", "-q", "80"]));
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("for lossy output"), "{err}");
    assert!(err.contains("`native-webp`"), "{err}");

    // No build adds lossless JPEG, so no feature is named.
    let (code, _, err) = run(sb.sqzer().args(["in.jpg", "-f", "jpeg", "--lossless"]));
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("no build offers lossless JPEG"), "{err}");
    assert!(!err.contains("native-jpegli"), "{err}");
}

// ---- Placement

#[test]
fn output_never_overwrites_input_without_in_place() {
    let sb = Sandbox::new("overwrite");
    let input = sb.fixture("pattern-rgb.jpg", "in.jpg");
    let original = fs::read(&input).unwrap();
    let (code, _, err) = run(sb
        .sqzer()
        .args(["in.jpg", "-f", "jpeg", "-q", "50", "--force"]));
    assert_eq!(code, 1);
    assert!(err.contains("would overwrite input"), "{err}");
    assert_eq!(fs::read(&input).unwrap(), original);
    // An existing output is refused too.
    fs::write(sb.path("in.avif"), b"precious").unwrap();
    let (code, _, err) = run(sb.sqzer().args(["in.jpg", "-f", "avif", "-q", "50"]));
    assert_eq!(code, 1);
    assert!(err.contains("output exists"), "{err}");
    assert_eq!(fs::read(sb.path("in.avif")).unwrap(), b"precious");
}

#[test]
fn in_place_with_backup_keeps_the_original() {
    let sb = Sandbox::new("inplace");
    let input = sb.fixture("pattern-rgb.jpg", "photo.jpg");
    let original = fs::read(&input).unwrap();
    let (code, out, err) = run(sb.sqzer().args([
        "photo.jpg",
        "--in-place",
        "--backup",
        "-f",
        "jpeg",
        "-q",
        "30",
        "--force",
        "--json",
    ]));
    assert_eq!(code, 0, "{err}");
    assert_eq!(json_lines(&out)[0]["output"], "photo.jpg");
    let replaced = fs::read(&input).unwrap();
    assert!(is_jpeg(&replaced));
    assert_ne!(replaced, original);
    assert_eq!(fs::read(sb.path("photo@backup.jpg")).unwrap(), original);
    // In place with a format change is refused per file.
    let (code, _, err) = run(sb.sqzer().args(["photo.jpg", "--in-place", "-f", "avif"]));
    assert_eq!(code, 1);
    assert!(
        err.contains("--in-place would turn JPEG into AVIF"),
        "{err}"
    );
}

#[test]
fn output_file_suffix_and_template() {
    let sb = Sandbox::new("naming");
    sb.fixture("pattern-rgb.webp", "in.webp");
    let (code, _, err) = run(sb.sqzer().args([
        "in.webp",
        "-f",
        "png",
        "--lossless",
        "--force",
        "-o",
        "sub/named.png",
    ]));
    assert_eq!(code, 0, "{err}");
    assert!(is_png(&fs::read(sb.path("sub/named.png")).unwrap()));
    let (code, _, err) = run(sb.sqzer().args([
        "in.webp",
        "-f",
        "png",
        "--lossless",
        "--force",
        "--suffix",
        "-min",
    ]));
    assert_eq!(code, 0, "{err}");
    assert!(sb.path("in-min.png").exists());
    let (code, _, err) = run(sb.sqzer().args([
        "in.webp",
        "-f",
        "png",
        "--lossless",
        "--force",
        "--template",
        "{stem}-{width}w-{quality}.{ext}",
    ]));
    assert_eq!(code, 0, "{err}");
    assert!(sb.path("in-48w-lossless.png").exists());
}

// ---- Paths

#[test]
fn path_corpus_of_awkward_names() {
    let sb = Sandbox::new("paths");
    let mut names = vec![
        "a[1].webp",
        "dots.in.name.webp",
        "UPPER.WEBP",
        "图片.webp",
        "-dash.webp",
        "with space.webp",
    ];
    if !cfg!(windows) {
        names.push("trailing .webp");
    }
    for n in &names {
        sb.fixture("pattern-rgb.webp", n);
    }
    let mut cmd = sb.sqzer();
    cmd.args(["-f", "png", "--lossless", "--force", "--json", "--"]);
    cmd.args(&names);
    // A trailing quote, as cmd.exe leaves it, is stripped.
    cmd.arg("a[1].webp\"");
    let (code, out, err) = run(&mut cmd);
    assert_eq!(code, 0, "{err}");
    let lines = json_lines(&out);
    assert_eq!(lines.len(), names.len(), "{out}");
    assert!(lines.iter().all(|l| l["status"] == "written"), "{out}");
    assert!(sb.path("a[1].png").exists());
    assert!(
        sb.path("dots.in.name.png").exists(),
        "stem is everything before the last dot"
    );
    assert!(sb.path("UPPER.png").exists());
    assert!(sb.path("图片.png").exists());
    assert!(sb.path("-dash.png").exists());
    assert!(sb.path("with space.png").exists());
}

#[test]
fn globs_expand_in_process_case_insensitively() {
    let sb = Sandbox::new("glob");
    sb.fixture("pattern-rgb.webp", "UPPER.WEBP");
    sb.fixture("pattern-rgb.webp", "lower.webp");
    sb.fixture("pattern-rgb.jpg", "other.jpg");
    // Passed as one literal argument: no shell expands it here.
    let (code, out, err) =
        run(sb
            .sqzer()
            .args(["*.webp", "-f", "png", "--lossless", "--force", "--json"]));
    assert_eq!(code, 0, "{err}");
    let mut inputs: Vec<&str> = Vec::new();
    let lines = json_lines(&out);
    for l in &lines {
        inputs.push(l["input"].as_str().unwrap());
    }
    inputs.sort_unstable();
    assert_eq!(inputs, vec!["UPPER.WEBP", "lower.webp"]);
}

#[test]
fn files_from_and_nul_separation() {
    let sb = Sandbox::new("filesfrom");
    sb.fixture("pattern-rgb.webp", "a.webp");
    sb.fixture("pattern-rgb.webp", "b c.webp");
    fs::write(sb.path("list"), b"a.webp\0b c.webp\0").unwrap();
    let (code, out, err) = run(sb.sqzer().args([
        "--files-from",
        "list",
        "-0",
        "-f",
        "png",
        "--lossless",
        "--force",
        "--json",
    ]));
    assert_eq!(code, 0, "{err}");
    assert_eq!(json_lines(&out).len(), 2);
    assert!(sb.path("b c.png").exists());
    let (code, _, err) = run(sb.sqzer().args(["--files-from", "nolist", "-f", "png"]));
    assert_eq!(code, 2, "{err}");
}

#[test]
fn stdin_to_stdout() {
    let sb = Sandbox::new("stdin");
    let bytes = fs::read(sb.fixture("pattern-rgb.webp", "src.webp")).unwrap();
    let (code, out, err) =
        run_with_stdin(sb.sqzer().args(["-", "-f", "png", "--lossless"]), &bytes);
    assert_eq!(code, 0, "{err}");
    assert!(is_png(&out), "stdout is the image");
    // With -o the image goes to the file and --json is allowed.
    let (code, out, err) = run_with_stdin(
        sb.sqzer().args([
            "-",
            "-f",
            "png",
            "--lossless",
            "-o",
            "from-stdin.png",
            "--json",
        ]),
        &bytes,
    );
    assert_eq!(code, 0, "{err}");
    let lines = json_lines(&String::from_utf8_lossy(&out));
    assert_eq!(lines[0]["input"], "-");
    assert_eq!(lines[0]["output"], "from-stdin.png");
    assert!(is_png(&fs::read(sb.path("from-stdin.png")).unwrap()));
}

// ---- Feedback

#[test]
fn dry_run_plans_and_writes_nothing() {
    let sb = Sandbox::new("dryrun");
    sb.fixture("pattern-rgba.webp", "in.webp");
    let (code, out, err) = run(sb.sqzer().args(["in.webp", "-n", "--json"]));
    assert_eq!(code, 0, "{err}");
    let r = &json_lines(&out)[0];
    assert_eq!(r["status"], "planned");
    assert_eq!(r["width"], 48);
    assert_eq!(r["height"], 32);
    assert_eq!(r["output_width"], 48);
    assert_eq!(r["output_height"], 32);
    assert_eq!(r["alpha"], true);
    assert_eq!(r["input_format"], "webp");
    assert_eq!(r["format"], "avif");
    assert_eq!(r["output"], "in.avif");
    assert!(r.get("output_bytes").is_none());
    assert!(!sb.path("in.avif").exists());
    let (code, _, err) = run(sb.sqzer().args(["in.webp", "-n", "--progress", "always"]));
    assert_eq!(code, 0);
    assert!(err.contains("48x32 alpha"), "{err}");
    assert!(err.contains("-> in.avif (avif)"), "{err}");
}

#[test]
fn progress_quiet_and_verbose_levels() {
    let sb = Sandbox::new("feedback");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    let (code, out, err) = run(sb.sqzer().args([
        "in.jpg", "-f", "jpeg", "-q", "40", "-o", "q.jpg", "--force", "--quiet",
    ]));
    assert_eq!(code, 0);
    assert!(
        out.is_empty() && err.is_empty(),
        "quiet success says nothing: {out}{err}"
    );
    let (code, out, err) = run(sb.sqzer().args([
        "in.jpg",
        "-f",
        "avif",
        "-o",
        "p.avif",
        "--force",
        "--progress",
        "always",
        "-vv",
    ]));
    assert_eq!(code, 0, "{err}");
    assert!(out.is_empty(), "no --json, nothing on stdout: {out}");
    assert!(err.contains("in.jpg -> p.avif"), "{err}");
    assert!(err.contains("trials:"), "{err}");
    let backend = if cfg!(feature = "native") {
        "params: backend libavif (native)"
    } else {
        "params: backend ravif (portable)"
    };
    assert!(err.contains(backend), "{err}");
    assert!(
        !err.contains("written"),
        "one output, no summary line: {err}"
    );
}

#[test]
fn summary_line_closes_a_batch() {
    let sb = Sandbox::new("summary");
    sb.fixture("pattern-rgb.jpg", "a.jpg");
    sb.fixture("pattern-rgb.jpg", "longer-name.jpg");
    fs::write(sb.path("bad.jpg"), b"nope").unwrap();
    let (code, _, err) = run(sb.sqzer().args([
        "a.jpg",
        "longer-name.jpg",
        "bad.jpg",
        "-f",
        "jpeg",
        "-q",
        "30",
        "--suffix",
        "-min",
        "--force",
        "--progress",
        "always",
    ]));
    assert_eq!(code, 1);
    let last = err.trim_end().lines().last().unwrap();
    assert!(last.starts_with("2 written, 1 failed"), "{err}");
    assert!(last.contains(" s"), "elapsed time: {err}");
    // Columns: the short name is padded to the long one.
    assert!(err.contains("a.jpg           -> a-min.jpg"), "{err}");
    let (_, _, err) =
        run(sb
            .sqzer()
            .args(["a.jpg", "longer-name.jpg", "-n", "--progress", "always"]));
    assert!(
        err.trim_end()
            .lines()
            .last()
            .unwrap()
            .starts_with("2 planned"),
        "{err}"
    );
}

#[test]
fn colour_is_off_by_default_here_and_on_when_asked() {
    let sb = Sandbox::new("color");
    let (_, _, err) = run(Command::new(env!("CARGO_BIN_EXE_sqzer"))
        .current_dir(&sb.dir)
        .args(["missing.jpg", "--color", "never"]));
    assert!(!err.contains('\x1b'), "{err:?}");
    let (_, _, err) = run(Command::new(env!("CARGO_BIN_EXE_sqzer"))
        .current_dir(&sb.dir)
        .args(["missing.jpg", "--color", "always"]));
    assert!(
        err.contains("\x1b[1m\x1b[31merror:\x1b[0m"),
        "clap's style: {err:?}"
    );
    // Errors from argument checks and from the build are styled the same way.
    let (_, _, err) = run(Command::new(env!("CARGO_BIN_EXE_sqzer"))
        .current_dir(&sb.dir)
        .args(["--list-codecs", "--bogus", "--color", "always"]));
    assert!(err.contains("\x1b[1m\x1b[31merror:\x1b[0m"), "{err:?}");
    let (_, _, err) = run(Command::new(env!("CARGO_BIN_EXE_sqzer"))
        .current_dir(&sb.dir)
        .args(["x.jpg", "-x", "jpeg:nope=1", "--color", "always"]));
    assert!(err.contains("\x1b[1m\x1b[31merror:\x1b[0m"), "{err:?}");
    let (_, _, err) = run(Command::new(env!("CARGO_BIN_EXE_sqzer"))
        .current_dir(&sb.dir)
        .env("NO_COLOR", "1")
        .args(["missing.jpg", "--color", "auto"]));
    assert!(!err.contains('\x1b'), "{err:?}");
}

#[test]
fn help_tiers_and_version() {
    let sb = Sandbox::new("help");
    let (code, short, _) = run(sb.sqzer().arg("-h"));
    assert_eq!(code, 0);
    let (code, long, _) = run(sb.sqzer().arg("--help"));
    assert_eq!(code, 0);
    assert!(short.contains("--target"), "{short}");
    assert!(
        !short.contains("-x, --codec-opt"),
        "-h hides the advanced flags: {short}"
    );
    assert!(long.contains("-x, --codec-opt"), "{long}");
    assert!(!short.contains("--files-from") && long.contains("--files-from"));
    assert!(short.contains("sqzer --list-codecs") && long.contains("sqzer --list-codecs"));
    let (code, out, _) = run(sb.sqzer().arg("--version"));
    assert_eq!(code, 0);
    assert_eq!(out.trim(), format!("sqzer {}", env!("CARGO_PKG_VERSION")));
}

// ---- Flags reach the encoder as given

#[test]
fn quality_and_effort_change_the_output() {
    let sb = Sandbox::new("mapping");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    for (q, name) in [("90", "hi.jpg"), ("20", "lo.jpg")] {
        let (code, _, err) = run(sb
            .sqzer()
            .args(["in.jpg", "-f", "jpeg", "-q", q, "-o", name, "--force"]));
        assert_eq!(code, 0, "{err}");
    }
    let hi = fs::metadata(sb.path("hi.jpg")).unwrap().len();
    let lo = fs::metadata(sb.path("lo.jpg")).unwrap().len();
    assert!(
        hi > lo,
        "q90 ({hi} bytes) must be larger than q20 ({lo} bytes)"
    );
    // A codec option reaches its backend. PNG stays portable in every
    // build, so its option set is the same everywhere.
    let (code, _, err) = run(sb.sqzer().args([
        "in.jpg",
        "-f",
        "png",
        "--lossless",
        "-x",
        "png:interlace=false",
        "-o",
        "plain.png",
        "--force",
    ]));
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = run(sb.sqzer().args([
        "in.jpg",
        "-f",
        "png",
        "--lossless",
        "-x",
        "png:interlace=true",
        "-o",
        "adam7.png",
        "--force",
    ]));
    assert_eq!(code, 0, "{err}");
    assert_ne!(
        fs::read(sb.path("plain.png")).unwrap(),
        fs::read(sb.path("adam7.png")).unwrap()
    );
}

#[test]
fn unsupported_settings_are_refused_not_approximated() {
    let sb = Sandbox::new("unsupported");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    // Neither AVIF backend can embed a profile, so `--keep-icc` on a
    // tagged input is refused per file rather than re-tagged as sRGB.
    sb.fixture("pattern-icc.jpg", "tagged.jpg");
    let (code, out, err) = run(sb.sqzer().args([
        "tagged.jpg",
        "-f",
        "avif",
        "-q",
        "50",
        "--keep-icc",
        "--json",
    ]));
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("does not support an embedded ICC profile"),
        "{err}"
    );
    assert_eq!(json_lines(&out)[0]["status"], "failed");
    assert!(!sb.path("tagged.avif").exists());
    // A bad option value is the backend's error, per file. Both AVIF
    // backends take `alpha_quality`.
    let (code, _, err) = run(sb.sqzer().args([
        "in.jpg",
        "-f",
        "avif",
        "-q",
        "50",
        "-x",
        "avif:alpha_quality=lots",
    ]));
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("avif:alpha_quality expects"), "{err}");
}

#[test]
fn fast_mode_skips_the_search() {
    let sb = Sandbox::new("fast");
    sb.fixture("pattern-rgb.jpg", "in.jpg");
    // Both tiers have a JPEG table, but WebP is the format that exists in
    // the native tier only, so it doubles as a check that the native table
    // is wired up.
    let (format, out_name) = if cfg!(feature = "native") {
        ("webp", "fast.webp")
    } else {
        ("jpeg", "fast.jpg")
    };
    let (code, out, err) = run(sb.sqzer().args([
        "in.jpg", "--fast", "-f", format, "-o", out_name, "--force", "--json",
    ]));
    assert_eq!(code, 0, "{err}");
    let r = &json_lines(&out)[0];
    assert_eq!(r["status"], "written");
    assert!(r["quality"].is_number(), "{r}");
    assert!(r.get("iterations").is_none(), "no search ran: {r}");
}

#[test]
fn rimage_syntax_gets_a_rewrite_hint() {
    let sb = Sandbox::new("rimage");
    // The hint fires on the first argument, as `rimage` always had the codec there.
    let (code, _, err) = run(Command::new(env!("CARGO_BIN_EXE_sqzer"))
        .current_dir(&sb.dir)
        .args(["mozjpeg", "-q", "75", "-d", "out", "in.jpg"]));
    assert_eq!(code, 2);
    assert!(err.contains("rimage syntax"), "{err}");
    assert!(err.contains("sqzer -f jpeg"), "{err}");
    assert!(err.contains("-o out"), "{err}");
}
