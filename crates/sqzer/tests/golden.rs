//! Golden SSIMULACRA2 scores per encoder: the test pattern encoded at a
//! fixed quality must score within a tolerance of a committed value, so a
//! dependency bump that degrades an encoder's output fails CI.
//!
//! The reference is `pattern-rgb.webp`, a lossless WebP of the 48 x 32
//! pattern described in `tests/fixtures/README.md`, decoded through the
//! registry. Lossless encoders must round-trip it to a score of 100.
//!
//! Every compiled-in encoder is checked, including a portable one that a
//! native backend has taken over, so the table is keyed by backend name.
//! A lossy encoder without a golden fails: add the value, do not skip it.
//!
//! Updating a golden value is a deliberate act: state the dependency bump
//! and the before and after scores in the PR.

#![cfg(feature = "portable")]

use std::path::PathBuf;

use sqzer::core::codec::Encoder;
use sqzer::core::image::Image;
use sqzer::core::metric::Metric;
use sqzer::core::params::{DecodeOpts, EncodeParams, Target};
use sqzer::metrics::Ssimulacra2;

/// Backend name, abstract quality, committed score.
const GOLDEN: &[(&str, f32, f32)] = &[
    ("mozjpeg-rs", 75.0, 51.8),
    ("ravif", 75.0, 88.1),
    // Native tier, measured on x86_64 Linux with libwebp 1.6 (libwebp-sys
    // 0.14.4), libjxl 0.12.0, libaom 3.11 and libjxl 0.10.2's jpegli.
    //
    // libwebp's score looks broken and is not: at q75 its plain RGB to YUV
    // downsampling smears the pattern's hard blue edge, which the metric
    // punishes on a 48 x 32 image (mean absolute error is 3.9, in line
    // with the others). `webp:sharp_yuv=true` scores 54 at the same
    // quality. The number is a regression check, not a quality claim.
    ("webpx", 75.0, 7.6),
    ("gamut-jxl", 75.0, 68.4),
    ("libavif", 75.0, 67.4),
    ("jpegli", 75.0, 54.2),
];

/// Scores from the SIMD paths of `fast-ssim2` on different targets, and
/// from the encoders' own SIMD or assembly paths on different targets,
/// agree to well under this. A regression worth catching is larger.
const TOLERANCE: f32 = 1.5;

fn reference() -> Image {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pattern-rgb.webp");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    sqzer::codecs::registry()
        .decode(&bytes, &DecodeOpts::default())
        .unwrap()
        .image
}

fn score_at(enc: &dyn Encoder, quality: f32) -> f32 {
    let reg = sqzer::codecs::registry();
    let src = reference();
    let params = EncodeParams {
        target: Target::Quality(quality),
        ..Default::default()
    };
    let bytes = enc.encode(&src, &params).unwrap();
    let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
    Ssimulacra2.score(&src, &back).unwrap()
}

#[test]
fn lossy_encoders_hold_their_golden_scores() {
    let reg = sqzer::codecs::registry();
    let mut failures = Vec::new();
    for enc in reg.encoders().filter(|e| e.caps().lossy) {
        let caps = enc.caps();
        let name = caps.name;
        if !reg.has_decoder(caps.format) {
            eprintln!("{name}: skipped, this build cannot decode {}", caps.format);
            continue;
        }
        let Some(&(_, quality, expected)) = GOLDEN.iter().find(|g| g.0 == name) else {
            failures.push(format!("{name}: no golden score committed"));
            continue;
        };
        let got = score_at(enc, quality);
        eprintln!("{name} q{quality}: {got:.3} (golden {expected})");
        if (got - expected).abs() > TOLERANCE {
            failures.push(format!(
                "{name} at q{quality}: scored {got:.3}, golden is {expected} +/- {TOLERANCE}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn lossless_encoders_round_trip_to_100() {
    let reg = sqzer::codecs::registry();
    let src = reference();
    let params = EncodeParams {
        target: Target::Lossless,
        ..Default::default()
    };
    for enc in reg.encoders().filter(|e| e.caps().lossless) {
        let name = enc.caps().name;
        let bytes = enc.encode(&src, &params).unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        let s = Ssimulacra2.score(&src, &back).unwrap();
        assert!(s > 99.99, "{name}: {s}");
    }
}
