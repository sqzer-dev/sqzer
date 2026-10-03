//! The package as JavaScript calls it, run by `wasm-pack test --node`
//! (ADR-0011 item 3). Options go in as the objects a caller would write
//! and results are read back as the objects a caller would get. wasm32 has
//! no file system, so the fixtures are compiled in.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, JSON, Reflect, Uint8Array};
use sqzer::core::codec::Encoder;
use sqzer::core::metric::Metric;
use sqzer::core::params::{DecodeOpts, EncodeParams, Target};
use sqzer::metrics::Ssimulacra2;
use wasm_bindgen_test::wasm_bindgen_test;

use super::*;

macro_rules! fixtures {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../../../tests/fixtures/", $name)).as_slice())),*]
    };
}

/// Every fixture the package's own decoders read: all of them but the SVG
/// and HEIC ones. A fixture added to the folder is added here.
const RASTER: &[(&str, &[u8])] = fixtures![
    "pattern-10bit.avif",
    "pattern-alpha.gif",
    "pattern-anim.gif",
    "pattern-anim.webp",
    "pattern-ascii.ppm",
    "pattern-container.jxl",
    "pattern-gray.avif",
    "pattern-gray.exr",
    "pattern-gray.jpg",
    "pattern-gray.jxl",
    "pattern-gray.pgm",
    "pattern-gray.tga",
    "pattern-gray.tif",
    "pattern-icc.avif",
    "pattern-icc.jpg",
    "pattern-icc.jxl",
    "pattern-icc.tif",
    "pattern-icc.webp",
    "pattern-lossy-alpha.webp",
    "pattern-lossy.jxl",
    "pattern-lossy.webp",
    "pattern-meta.avif",
    "pattern-meta.jpg",
    "pattern-meta.jxl",
    "pattern-meta.tif",
    "pattern-meta.webp",
    "pattern-mirror.avif",
    "pattern-p3.avif",
    "pattern-rgb.avif",
    "pattern-rgb.bmp",
    "pattern-rgb.exr",
    "pattern-rgb.gif",
    "pattern-rgb.jpg",
    "pattern-rgb.jxl",
    "pattern-rgb.ppm",
    "pattern-rgb.qoi",
    "pattern-rgb.tga",
    "pattern-rgb.tif",
    "pattern-rgb.webp",
    "pattern-rgb16.jxl",
    "pattern-rgb16.ppm",
    "pattern-rgb16.tif",
    "pattern-rgba.avif",
    "pattern-rgba.bmp",
    "pattern-rgba.exr",
    "pattern-rgba.ico",
    "pattern-rgba.jxl",
    "pattern-rgba.qoi",
    "pattern-rgba.tga",
    "pattern-rgba.tif",
    "pattern-rgba.webp",
    "pattern-rot90.avif",
    "pattern-rot90.jpg",
    "pattern-rot90.tif",
    "pattern-rot90.webp",
];

/// The 48 x 32 pattern, lossless: the reference of the golden tests.
const RGB: &[u8] = include_bytes!("../../../tests/fixtures/pattern-rgb.webp");
const RGBA: &[u8] = include_bytes!("../../../tests/fixtures/pattern-rgba.webp");
const SVG: &[u8] = include_bytes!("../../../tests/fixtures/pattern-rgb.svg");
const HEIC: &[u8] = include_bytes!("../../../tests/fixtures/pattern-rgb.heic");

/// An options argument, from the JSON of the object a caller would pass.
// Every caller hands it over as the optional argument it is.
#[allow(clippy::unnecessary_wraps)]
fn options<T: Tsify>(json: &str) -> Option<Ts<T>> {
    Some(Ts::new_unchecked(JSON::parse(json).unwrap()))
}

fn get(object: &JsValue, key: &str) -> JsValue {
    Reflect::get(object, &key.into()).unwrap()
}

fn text(object: &JsValue, key: &str) -> String {
    get(object, key)
        .as_string()
        .unwrap_or_else(|| panic!("`{key}` is not a string"))
}

fn number(object: &JsValue, key: &str) -> f64 {
    get(object, key)
        .as_f64()
        .unwrap_or_else(|| panic!("`{key}` is not a number"))
}

fn bytes(output: &JsValue) -> Vec<u8> {
    let bytes = get(output, "bytes");
    assert!(
        bytes.is_instance_of::<Uint8Array>(),
        "`bytes` is a Uint8Array"
    );
    Uint8Array::from(bytes).to_vec()
}

fn strings(object: &JsValue, key: &str) -> Vec<String> {
    Array::from(&get(object, key))
        .iter()
        .map(|v| v.as_string().unwrap())
        .collect()
}

/// What a call threw, which must be a `SqzerError` of `kind`.
fn thrown<T>(result: Result<T, SqzerError>, kind: &str) -> JsValue {
    let Err(error) = result else {
        panic!("expected a {kind} error");
    };
    let error = JsValue::from(error);
    assert!(error.is_instance_of::<js_sys::Error>());
    assert_eq!(text(&error, "name"), "SqzerError");
    assert_eq!(text(&error, "kind"), kind, "{}", text(&error, "message"));
    error
}

fn optimized(input: &[u8], json: &str) -> JsValue {
    optimize(input, options(json))
        .unwrap_or_else(|e| panic!("{json}: {e:?}"))
        .js_value()
}

#[wasm_bindgen_test]
fn every_raster_fixture_decodes() {
    for &(name, input) in RASTER {
        let image = decode(input, None).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!((image.width(), image.height()), (48, 32), "{name}");
        let extension = name.rsplit('.').next().unwrap();
        let format = Format::from_extension(extension).map(format_name);
        assert_eq!(image.format().as_deref(), format, "{name}");
        assert_eq!(image.animated(), name.contains("anim"), "{name}");
        if name.contains("rgba") {
            assert!(image.alpha(), "{name}");
        } else if name.contains("rgb") || name.contains("gray") {
            assert!(!image.alpha(), "{name}");
        }
    }
}

#[wasm_bindgen_test]
fn avif_decodes_to_the_pattern() {
    let pattern = sqzer(None).decode(RGB).unwrap().image;
    let pattern = pattern.samples().as_u8().unwrap();
    for name in ["pattern-rgb.avif", "pattern-rot90.avif"] {
        let input = RASTER.iter().find(|f| f.0 == name).unwrap().1;
        let image = sqzer(None).decode(input).unwrap().image;
        let samples = image.samples().as_u8().unwrap();
        let sum: u32 = samples
            .iter()
            .zip(pattern)
            .map(|(a, b)| u32::from(a.abs_diff(*b)))
            .sum();
        let mean = f64::from(sum) / samples.len() as f64;
        assert!(mean < 4.0, "{name}: mean difference {mean} to the pattern");
    }
}

#[wasm_bindgen_test]
fn every_encoder_writes_what_decodes_back() {
    for (format, backend, searched) in [
        ("jpeg", "mozjpeg-rs", true),
        ("png", "oxipng", false),
        ("webp", "image-webp", false),
        ("avif", "ravif", true),
    ] {
        let out = optimized(RGB, &format!(r#"{{"format":"{format}"}}"#));
        assert_eq!(text(&out, "format"), format);
        assert_eq!(text(&out, "backend"), backend);
        assert_eq!(text(&out, "tier"), "portable");
        assert_eq!(text(&out, "inputFormat"), "webp");
        assert_eq!(get(&out, "animated"), false);
        assert_eq!(get(&out, "alpha"), false);
        assert_eq!(
            (number(&out, "width"), number(&out, "height")),
            (48.0, 32.0)
        );
        assert_eq!(number(&out, "outputWidth"), 48.0);
        assert_eq!(number(&out, "outputHeight"), 32.0);
        assert_eq!(number(&out, "target"), 70.0);
        assert_eq!(get(&out, "lossless"), !searched, "{format}");
        if searched {
            assert_eq!(get(&out, "reached"), true, "{format}");
            assert!(number(&out, "score") >= 68.0, "{format}");
            let trials = Array::from(&get(&out, "trials"));
            assert_eq!(f64::from(trials.length()), number(&out, "iterations"));
            // The last trial is not always the chosen one, but the chosen
            // quality is one that was tried.
            let quality = number(&out, "quality");
            assert!(trials.iter().any(|t| number(&t, "quality") == quality));
        } else {
            // Keys that do not apply are absent, not null.
            for key in [
                "quality",
                "score",
                "reached",
                "capped",
                "iterations",
                "trials",
            ] {
                assert!(!Reflect::has(&out, &key.into()).unwrap(), "{format} {key}");
            }
        }
        let back = decode(&bytes(&out), None).unwrap();
        assert_eq!((back.width(), back.height()), (48, 32), "{format}");
        assert_eq!(back.format().as_deref(), Some(format));
    }
}

#[wasm_bindgen_test]
fn the_default_is_the_command_lines() {
    // No options: a photograph goes to AVIF, searched to a score of 70.
    let out = optimize(RGB, None).unwrap().js_value();
    assert_eq!(text(&out, "content"), "photo");
    assert_eq!(text(&out, "format"), "avif");
    assert_eq!(number(&out, "target"), 70.0);
    assert_eq!(get(&out, "reached"), true);
    // An explicit quality is not searched, and crosses as the number given.
    let out = optimized(RGB, r#"{"format":"jpeg","quality":62.5}"#);
    assert_eq!(number(&out, "quality"), 62.5);
    assert!(!Reflect::has(&out, &"target".into()).unwrap());
    assert!(!Reflect::has(&out, &"score".into()).unwrap());
    // An `f32` crosses as the decimal it prints as.
    assert_eq!(tidy(51.8), 51.8);
    assert_ne!(f64::from(51.8f32), 51.8);
}

#[wasm_bindgen_test]
fn a_missing_backend_names_the_builds_that_have_it() {
    let error = thrown(
        optimize(RGB, options(r#"{"format":"jxl","quality":50}"#)),
        "EncoderUnavailable",
    );
    assert_eq!(strings(&error, "availableIn"), ["native-jxl"]);
    assert!(text(&error, "message").contains("no encoder for JPEG XL"));
    // Recognised input without a decoder says so, for `decodeAny` to act on.
    let error = thrown(decode(HEIC, None), "DecoderUnavailable");
    assert_eq!(strings(&error, "availableIn"), ["native-heif"]);
    let error = thrown(decode(SVG, None), "DecoderUnavailable");
    assert_eq!(strings(&error, "availableIn"), ["svg"]);
    let error = thrown(decode(b"definitely not an image", None), "UnknownFormat");
    assert!(get(&error, "availableIn").is_undefined());
}

#[wasm_bindgen_test]
fn on_trial_is_called_once_per_trial() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let on_trial = Closure::<dyn FnMut(JsValue)>::new(move |trial| sink.borrow_mut().push(trial));
    let with = |callback: &JsValue| {
        let object = JSON::parse(r#"{"format":"jpeg"}"#).unwrap();
        Reflect::set(&object, &"onTrial".into(), callback).unwrap();
        Some(Ts::<Options>::new_unchecked(object))
    };
    let out = optimize(RGB, with(on_trial.as_ref())).unwrap().js_value();
    let trials = Array::from(&get(&out, "trials"));
    assert_eq!(seen.borrow().len(), trials.length() as usize);
    for (i, (called, listed)) in seen.borrow().iter().zip(trials.iter()).enumerate() {
        assert_eq!(number(called, "n"), (i + 1) as f64);
        assert_eq!(number(called, "max"), 6.0);
        assert_eq!(number(called, "quality"), number(&listed, "quality"));
        assert_eq!(number(called, "score"), number(&listed, "score"));
    }
    // No search, no call.
    let before = seen.borrow().len();
    let png = JSON::parse(r#"{"format":"png"}"#).unwrap();
    Reflect::set(&png, &"onTrial".into(), on_trial.as_ref()).unwrap();
    let image = decode(RGB, None).unwrap();
    image.encode(Some(Ts::new_unchecked(png))).unwrap();
    assert_eq!(seen.borrow().len(), before);
    // What `onTrial` throws comes out of the call, as it was thrown.
    let throws = Function::new_with_args("trial", "throw new RangeError('stop')");
    let Err(error) = optimize(RGB, with(&throws)) else {
        panic!("the exception was swallowed");
    };
    let error = JsValue::from(error);
    assert_eq!(text(&error, "name"), "RangeError");
    assert_eq!(text(&error, "message"), "stop");
    // And one that is not a function is refused before any work.
    let error = thrown(optimize(&[], with(&JsValue::from(1))), "InvalidParams");
    assert!(text(&error, "message").contains("`onTrial`"));
}

#[wasm_bindgen_test]
fn max_pixels_refuses_a_25_megapixel_header() {
    // A PPM header that promises 5000 x 5000 and delivers nothing: the
    // limit is checked on the header, before any pixel is allocated.
    let header = b"P6\n5000 5000\n255\n";
    let error = thrown(decode(header, None), "TooLarge");
    assert_eq!(
        text(&error, "message"),
        "image has 25000000 pixels, limit is 24000000"
    );
    thrown(optimize(header, None), "TooLarge");
    // The key moves the limit both ways.
    thrown(decode(RGB, options(r#"{"maxPixels":1535}"#)), "TooLarge");
    decode(RGB, options(r#"{"maxPixels":1536}"#)).unwrap();
    thrown(optimize(RGB, options(r#"{"maxPixels":1535}"#)), "TooLarge");
    // It bounds what a resize may make as well.
    let image = decode(RGB, None).unwrap();
    let error = thrown(
        image.encode(options(
            r#"{"format":"png","scale":4,"enlarge":true,"maxPixels":2000}"#,
        )),
        "InvalidParams",
    );
    assert!(text(&error, "message").contains("192x128"));
}

#[wasm_bindgen_test]
fn from_pixels_round_trips_a_fixture() {
    let decoded = sqzer(None).decode(RGBA).unwrap().image;
    let rgba = decoded.samples().as_u8().unwrap();
    let image = from_pixels(rgba, 48, 32, None).unwrap();
    assert_eq!((image.width(), image.height()), (48, 32));
    assert!(image.alpha());
    assert!(!image.animated());
    assert_eq!(image.format(), None);
    let out = image
        .encode(options(r#"{"format":"png","lossless":true}"#))
        .unwrap()
        .js_value();
    assert!(!Reflect::has(&out, &"inputFormat".into()).unwrap());
    let back = sqzer(None).decode(&bytes(&out)).unwrap().image;
    assert_eq!(back.samples().as_u8().unwrap(), rgba);

    // Opaque pixels, which is every canvas of a photograph: no alpha
    // channel, so an encoder is not asked to write one.
    let opaque: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2], 255])
        .collect();
    let image = from_pixels(&opaque, 48, 32, None).unwrap();
    assert!(!image.alpha());
    let out = image
        .encode(options(r#"{"format":"png","lossless":true}"#))
        .unwrap()
        .js_value();
    let back = sqzer(None).decode(&bytes(&out)).unwrap().image;
    let rgb = sqzer(None).decode(RGB).unwrap().image;
    assert_eq!(back.samples(), rgb.samples());

    let error = thrown(from_pixels(&opaque[1..], 48, 32, None), "InvalidInput");
    assert!(text(&error, "message").contains("6144 bytes, got 6143"));
    // The pixel limit holds here as it does for `decode`, on the size
    // claimed, before the pixels are looked at.
    let error = thrown(from_pixels(&[], 5000, 5000, None), "TooLarge");
    assert_eq!(
        text(&error, "message"),
        "image has 25000000 pixels, limit is 24000000"
    );
    thrown(
        from_pixels(&opaque, 48, 32, options(r#"{"maxPixels":1535}"#)),
        "TooLarge",
    );
    from_pixels(&opaque, 48, 32, options(r#"{"maxPixels":1536}"#)).unwrap();
}

#[wasm_bindgen_test]
fn one_decode_serves_many_encodes() {
    let image = decode(RGB, None).unwrap();
    let small = r#"{"format":"jpeg","quality":80,"width":24}"#;
    let a = image.encode(options(small)).unwrap().js_value();
    assert_eq!((number(&a, "width"), number(&a, "height")), (48.0, 32.0));
    assert_eq!(number(&a, "outputWidth"), 24.0);
    assert_eq!(number(&a, "outputHeight"), 16.0);
    // The same options again reuse the resized image, another quality
    // does too, and both agree with the one-call form.
    let b = image.encode(options(small)).unwrap().js_value();
    assert_eq!(bytes(&a), bytes(&b));
    assert_eq!(bytes(&a), bytes(&optimized(RGB, small)));
    let lower = r#"{"format":"jpeg","quality":40,"width":24}"#;
    let c = image.encode(options(lower)).unwrap().js_value();
    assert_eq!(bytes(&c), bytes(&optimized(RGB, lower)));
    assert_ne!(bytes(&c), bytes(&a));
    // Other options start from the decoded image again.
    let full = r#"{"format":"png","lossless":true}"#;
    let d = image.encode(options(full)).unwrap().js_value();
    assert_eq!(number(&d, "outputWidth"), 48.0);
    assert_eq!(bytes(&d), bytes(&optimized(RGB, full)));
}

#[wasm_bindgen_test]
fn the_resize_keys_are_the_resize_flags() {
    let size = |json: &str| {
        let out = optimized(RGB, json);
        (number(&out, "outputWidth"), number(&out, "outputHeight"))
    };
    assert_eq!(size(r#"{"format":"png","scale":0.5}"#), (24.0, 16.0));
    assert_eq!(size(r#"{"format":"png","height":8}"#), (12.0, 8.0));
    // Nothing scales up unless asked.
    assert_eq!(size(r#"{"format":"png","width":96}"#), (48.0, 32.0));
    assert_eq!(
        size(r#"{"format":"png","width":96,"enlarge":true}"#),
        (96.0, 64.0)
    );
    assert_eq!(
        size(r#"{"format":"png","width":16,"height":16,"fit":"cover","position":"top-left"}"#),
        (16.0, 16.0)
    );
    // The thumbnail preset's box takes a size over it.
    assert_eq!(
        size(r#"{"format":"png","preset":"thumbnail","width":24}"#),
        (24.0, 16.0)
    );
    // A contain fit is padded with the colour named.
    let out = optimized(
        RGB,
        r##"{"format":"png","lossless":true,"width":32,"height":32,"fit":"contain","background":"#ff0000"}"##,
    );
    assert_eq!(number(&out, "outputHeight"), 32.0);
    let back = sqzer(None).decode(&bytes(&out)).unwrap().image;
    assert_eq!(&back.samples().as_u8().unwrap()[..3], &[255, 0, 0]);
}

#[wasm_bindgen_test]
fn options_are_checked_before_any_work() {
    // No input at all: every one of these is refused on the options alone.
    for (json, needle) in [
        (r#"{"qualty":80}"#, "unknown option `qualty`"),
        (r#"{"target":70,"quality":80}"#, "alternatives"),
        (r#"{"quality":80,"lossless":true}"#, "alternatives"),
        (r#"{"quality":101}"#, "`quality` is 0 to 100"),
        (r#"{"target":150}"#, "up to 100"),
        (r#"{"effort":11}"#, "`effort` is 0 to 10"),
        (r#"{"fast":true,"quality":50}"#, "`fast`"),
        (r#"{"position":"top"}"#, "`position`"),
        (r#"{"background":"white","width":8}"#, "`background`"),
        (
            r##"{"background":"#12","width":8,"height":8,"fit":"contain"}"##,
            "is not a colour",
        ),
        (r#"{"fit":"cover","width":8}"#, "needs both"),
        (r#"{"scale":0.5,"width":8}"#, "`scale`"),
        (r#"{"scale":0}"#, "positive"),
        (r#"{"width":0}"#, "at least one pixel"),
        (r#"{"codecOpts":{"progressive":"false"}}"#, "`codec:key`"),
        (r#"{"codecOpts":{"bmp:rle":"1"}}"#, "unknown codec `bmp`"),
        (r#"{"codecOpts":{"jpeg:nope":"1"}}"#, "mozjpeg-rs accepts"),
        (r#"{"codecOpts":{"jxl:effort":"1"}}"#, "no JPEG XL encoder"),
        (r#"{"format":"gif"}"#, "options:"),
        (r#"{"width":"8"}"#, "options:"),
        (r#""fast""#, "options:"),
    ] {
        let error = thrown(optimize(&[], options(json)), "InvalidParams");
        let message = text(&error, "message");
        assert!(message.contains(needle), "{json}: {message}");
    }
    thrown(decode(&[], options(r#"{"width":8}"#)), "InvalidParams");
    // `null` and `undefined` are no options, and so are their values.
    thrown(
        optimize(&[], Some(Ts::new_unchecked(JsValue::NULL))),
        "UnknownFormat",
    );
    let sparse = js_sys::eval("({ format: undefined, lossless: undefined })").unwrap();
    thrown(
        optimize(&[], Some(Ts::new_unchecked(sparse))),
        "UnknownFormat",
    );
    // A codec option reaches its backend.
    let plain = optimized(RGB, r#"{"format":"jpeg","quality":80}"#);
    let baseline = optimized(
        RGB,
        r#"{"format":"jpeg","quality":80,"codecOpts":{"jpeg:progressive":"false"}}"#,
    );
    assert_ne!(bytes(&plain), bytes(&baseline));
}

#[wasm_bindgen_test]
fn codecs_lists_this_build() {
    let listing = codecs().unwrap();
    assert_eq!(listing.len(), Format::ALL.len());
    let entry = |format: &str| {
        listing
            .iter()
            .map(Ts::js_value)
            .find(|c| text(c, "format") == format)
            .unwrap_or_else(|| panic!("no entry for {format}"))
    };
    let jpeg = entry("jpeg");
    assert_eq!(text(&jpeg, "extension"), "jpg");
    assert_eq!(text(&jpeg, "mime"), "image/jpeg");
    assert_eq!(text(&get(&jpeg, "decoder"), "backend"), "zune-jpeg");
    let encoder = get(&jpeg, "encoder");
    assert_eq!(text(&encoder, "backend"), "mozjpeg-rs");
    assert_eq!(text(&encoder, "tier"), "portable");
    assert_eq!(get(&encoder, "lossy"), true);
    assert_eq!(get(&encoder, "lossless"), false);
    assert!(Array::is_array(&get(&encoder, "bitDepth")));
    let keys: Vec<String> = Array::from(&get(&encoder, "options"))
        .iter()
        .map(|o| text(&o, "key"))
        .collect();
    assert!(keys.contains(&"jpeg:progressive".to_string()), "{keys:?}");
    // AVIF both ways, like every other build.
    let avif = entry("avif");
    assert_eq!(text(&get(&avif, "decoder"), "backend"), "rav1d");
    assert_eq!(text(&get(&avif, "encoder"), "backend"), "ravif");
    // The gaps: no JPEG XL encoder, and SVG and HEIC left to the browser.
    let jxl = entry("jxl");
    assert_eq!(text(&get(&jxl, "decoder"), "backend"), "jxl-oxide");
    assert!(!Reflect::has(&jxl, &"encoder".into()).unwrap());
    assert_eq!(strings(&jxl, "encoderFeatures"), ["native-jxl"]);
    for (format, feature) in [("svg", "svg"), ("heic", "native-heif")] {
        let gap = entry(format);
        assert!(!Reflect::has(&gap, &"decoder".into()).unwrap(), "{format}");
        assert_eq!(strings(&gap, "decoderFeatures"), [feature]);
    }
}

/// Backend, abstract quality, committed score: the portable rows of
/// `crates/sqzer/tests/golden.rs`. Change both together.
const GOLDEN: &[(&str, f32, f32)] = &[("mozjpeg-rs", 75.0, 51.8), ("ravif", 75.0, 88.1)];
/// `png:colors` value, committed score: the `PALETTE` rows of the same file.
const PALETTE: &[(&str, f32)] = &[("256", 68.3), ("16", -49.7)];
const TOLERANCE: f32 = 1.5;

fn round_trip_score(encoder: &dyn Encoder, target: Target) -> f32 {
    let registry = registry();
    let pattern = registry.decode(RGB, &DecodeOpts::default()).unwrap().image;
    let params = EncodeParams {
        target,
        ..Default::default()
    };
    let file = encoder.encode(&pattern, &params).unwrap();
    let back = registry.decode(&file, &DecodeOpts::default()).unwrap();
    Ssimulacra2.score(&pattern, &back.image).unwrap()
}

#[wasm_bindgen_test]
fn encoders_hold_their_golden_scores_on_wasm32() {
    let registry = registry();
    let mut lossy = 0;
    for encoder in registry.encoders() {
        let caps = encoder.caps();
        if caps.lossless {
            let score = round_trip_score(encoder, Target::Lossless);
            assert!(score > 99.99, "{}: {score}", caps.name);
        }
        if caps.lossy {
            let &(_, quality, golden) = GOLDEN
                .iter()
                .find(|g| g.0 == caps.name)
                .unwrap_or_else(|| panic!("{}: no golden score committed", caps.name));
            let score = round_trip_score(encoder, Target::Quality(quality));
            assert!(
                (score - golden).abs() <= TOLERANCE,
                "{} at q{quality}: scored {score}, golden is {golden} +/- {TOLERANCE}",
                caps.name
            );
            lossy += 1;
        }
    }
    assert_eq!(lossy, GOLDEN.len());
}

#[wasm_bindgen_test]
fn a_png_palette_is_reported_as_lossy() {
    let plain = optimized(RGBA, r#"{"format":"png"}"#);
    assert_eq!(get(&plain, "lossless"), true);

    let out = optimized(RGBA, r#"{"format":"png","codecOpts":{"png:colors":"16"}}"#);
    assert_eq!(get(&out, "lossless"), false);
    assert!(!Reflect::has(&out, &"quality".into()).unwrap());
    assert!(!Reflect::has(&out, &"score".into()).unwrap());
    let back = sqzer(None).decode(&bytes(&out)).unwrap().image;
    let colors: std::collections::HashSet<_> = back
        .samples()
        .as_u8()
        .unwrap()
        .chunks_exact(back.channels())
        .collect();
    assert!(colors.len() <= 16, "{} colours", colors.len());

    // Asked for next to `lossless`, it is a contradiction (ADR-0012 D4).
    let error = thrown(
        optimize(
            RGBA,
            options(r#"{"format":"png","lossless":true,"codecOpts":{"png:colors":"16"}}"#),
        ),
        "InvalidParams",
    );
    assert!(
        text(&error, "message").contains("png:colors=16"),
        "{}",
        text(&error, "message")
    );
}

#[wasm_bindgen_test]
fn png_palettes_hold_their_golden_scores_on_wasm32() {
    let registry = registry();
    let pattern = registry.decode(RGB, &DecodeOpts::default()).unwrap().image;
    let png = registry.encoder(Format::Png).unwrap();
    for &(colors, golden) in PALETTE {
        let params = EncodeParams {
            target: Target::Lossless,
            ..Default::default()
        }
        .with_codec_opt("png", "colors", colors);
        let file = png.encode(&pattern, &params).unwrap();
        let back = registry.decode(&file, &DecodeOpts::default()).unwrap();
        let score = Ssimulacra2.score(&pattern, &back.image).unwrap();
        assert!(
            (score - golden).abs() <= TOLERANCE,
            "png:colors={colors}: scored {score}, golden is {golden} +/- {TOLERANCE}"
        );
    }
}

/// A canvas that returns opaque white for whatever is drawn on it, and a
/// `createImageBitmap` that calls every blob a 100 x 50 image. Node has
/// neither; the real ones are checked in browsers (ADR-0011 item 5). This
/// and [`NO_CANVAS`] are the scripts the tests evaluate, with one flag reset;
/// all are fixed strings.
const FAKE_CANVAS: &str = r"
globalThis.createImageBitmap = async (blob) => {
  globalThis.__sqzerBlob = { type: blob.type, size: blob.size };
  return { width: 100, height: 50, close() { globalThis.__sqzerClosed = true; } };
};
globalThis.OffscreenCanvas = class {
  getContext() {
    return {
      drawImage() {},
      getImageData: (x, y, w, h) => ({ data: new Uint8ClampedArray(w * h * 4).fill(255) }),
    };
  }
};
";
const NO_CANVAS: &str = "delete globalThis.createImageBitmap; delete globalThis.OffscreenCanvas;";

#[wasm_bindgen_test]
async fn decode_any_asks_the_browser_only_for_what_the_package_cannot_read() {
    // Its own decoder first: no canvas is needed, or touched.
    let image = decode_any(RGBA.to_vec(), None).await.unwrap();
    assert_eq!(image.format().as_deref(), Some("webp"));
    assert!(image.alpha());
    thrown(
        decode_any(b"not an image".to_vec(), None).await,
        "UnknownFormat",
    );
    // Node has no canvas: the error is `decode`'s, with the reason added.
    let error = thrown(decode_any(SVG.to_vec(), None).await, "DecoderUnavailable");
    assert_eq!(strings(&error, "availableIn"), ["svg"]);
    let message = text(&error, "message");
    assert!(
        message.starts_with("no decoder for SVG in this build"),
        "{message}"
    );
    assert!(
        message.contains("the browser could not decode it either"),
        "{message}"
    );
    assert!(message.contains("createImageBitmap"), "{message}");

    js_sys::eval(FAKE_CANVAS).unwrap();
    // A vector image is drawn at its own size, or to fit the box given.
    let image = decode_any(SVG.to_vec(), None).await.unwrap();
    assert_eq!((image.width(), image.height()), (100, 50));
    assert_eq!(image.format().as_deref(), Some("svg"));
    assert!(!image.alpha());
    let blob = get(&js_sys::global(), "__sqzerBlob");
    assert_eq!(text(&blob, "type"), "image/svg+xml");
    assert_eq!(number(&blob, "size"), SVG.len() as f64);
    assert_eq!(get(&js_sys::global(), "__sqzerClosed"), true);
    let image = decode_any(SVG.to_vec(), options(r#"{"width":400}"#))
        .await
        .unwrap();
    assert_eq!((image.width(), image.height()), (400, 200));
    let image = decode_any(SVG.to_vec(), options(r#"{"width":400,"height":20}"#))
        .await
        .unwrap();
    assert_eq!((image.width(), image.height()), (40, 20));
    // The limit is on what would be drawn, and an image refused is
    // released, not left for the garbage collector.
    js_sys::eval("globalThis.__sqzerClosed = false").unwrap();
    thrown(
        decode_any(SVG.to_vec(), options(r#"{"width":400,"maxPixels":79999}"#)).await,
        "TooLarge",
    );
    assert_eq!(get(&js_sys::global(), "__sqzerClosed"), true);
    // Pixels are decoded at their own size, whatever box is given, and
    // the result encodes like any other image.
    let image = decode_any(HEIC.to_vec(), options(r#"{"width":10}"#))
        .await
        .unwrap();
    assert_eq!((image.width(), image.height()), (100, 50));
    assert_eq!(
        text(&get(&js_sys::global(), "__sqzerBlob"), "type"),
        "image/heic"
    );
    let out = image
        .encode(options(r#"{"format":"png","width":10}"#))
        .unwrap()
        .js_value();
    assert_eq!(text(&out, "inputFormat"), "heic");
    assert_eq!(number(&out, "outputWidth"), 10.0);
    js_sys::eval(NO_CANVAS).unwrap();
}
