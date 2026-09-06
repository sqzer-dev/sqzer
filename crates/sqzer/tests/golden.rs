//! Golden SSIMULACRA2 scores per encoder: the test pattern encoded at a
//! fixed quality must score within a tolerance of a committed value, so a
//! dependency bump that degrades an encoder's output fails CI.
//!
//! The reference is `pattern-rgb.webp`, a lossless WebP of the 48 x 32
//! pattern described in `tests/fixtures/README.md`, decoded through the
//! registry. Lossless encoders must round-trip it to a score of 100.
//!
//! Updating a golden value is a deliberate act: state the dependency bump
//! and the before and after scores in the PR.

#![cfg(feature = "portable")]

use std::path::PathBuf;

use sqzer::core::codec::Format;
use sqzer::core::image::Image;
use sqzer::core::metric::Metric;
use sqzer::core::params::{DecodeOpts, EncodeParams, Target};
use sqzer::metrics::Ssimulacra2;

/// Encoder, abstract quality, committed score.
const GOLDEN: &[(Format, f32, f32)] = &[(Format::Jpeg, 75.0, 51.8), (Format::Avif, 75.0, 88.1)];

/// Scores from the SIMD paths of `fast-ssim2` on different targets, and
/// from `rav1e`'s Rust fallbacks on different targets, agree to well under
/// this. A regression worth catching is larger.
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

fn score_at(format: Format, quality: f32) -> f32 {
    let reg = sqzer::codecs::registry();
    let src = reference();
    let params = EncodeParams {
        target: Target::Quality(quality),
        ..Default::default()
    };
    let bytes = reg.encoder(format).unwrap().encode(&src, &params).unwrap();
    let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
    Ssimulacra2.score(&src, &back).unwrap()
}

#[test]
fn lossy_encoders_hold_their_golden_scores() {
    let reg = sqzer::codecs::registry();
    let mut failures = Vec::new();
    for &(format, quality, expected) in GOLDEN {
        if !reg.has_encoder(format) || !reg.has_decoder(format) {
            eprintln!("{format}: skipped, not both encodable and decodable in this build");
            continue;
        }
        let got = score_at(format, quality);
        eprintln!("{format} q{quality}: {got:.3} (golden {expected})");
        if (got - expected).abs() > TOLERANCE {
            failures.push(format!(
                "{format} at q{quality}: scored {got:.3}, golden is {expected} +/- {TOLERANCE}"
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
        let format = enc.caps().format;
        let bytes = enc.encode(&src, &params).unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        let s = Ssimulacra2.score(&src, &back).unwrap();
        assert!(s > 99.99, "{format}: {s}");
    }
}
