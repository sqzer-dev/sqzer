//! The palette search of `png:colors=auto` on the failure modes the
//! target search is held to: tiny images, flat images, pure noise, and a
//! target out of reach at 256 colours (ADR-0013). Through the real PNG
//! encoder and metric, as `Sqzer::run` drives them.

#![cfg(feature = "portable")]
// Synthetic pixel data: the truncating casts are the point. Palette sizes
// are whole numbers, so exact float comparison is the intent.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]

use sqzer::Sqzer;
use sqzer::core::codec::{Encoder, Format};
use sqzer::core::image::{ColorType, Image};
use sqzer::core::params::{EncodeParams, Resolved, Target};
use sqzer::metrics::{Axis, SearchReport};

fn png(img: &Image) -> Vec<u8> {
    sqzer::codecs::oxipng::OxipngEncoder
        .encode(
            img,
            &EncodeParams {
                target: Target::Lossless,
                ..EncodeParams::default()
            },
        )
        .unwrap()
}

fn search(img: &Image, target: f32) -> SearchReport {
    let out = Sqzer::new()
        .format(Format::Png)
        .target(Target::Ssimulacra2(target))
        .codec_opt("png", "colors", "auto")
        .run(&png(img))
        .unwrap();
    assert_eq!(out.target, Resolved::Lossless);
    assert!(!out.lossless);
    let report = out.report.expect("a palette search reports");
    assert_eq!(report.axis, Axis::Colors("png:colors"));
    assert!(
        report.iterations >= 1 && report.iterations <= 6,
        "{report:?}"
    );
    assert_eq!(report.trials.len(), usize::from(report.iterations));
    for t in &report.trials {
        assert!(
            (2.0..=256.0).contains(&t.value) && t.value.fract() == 0.0,
            "{t:?}"
        );
    }
    assert!(
        report.trials.iter().any(|t| t.value == report.value),
        "the pick is one of the trials: {report:?}"
    );
    report
}

/// A pseudo-random RGB image: every pixel its own colour, as near as 8
/// bits allow.
fn noise(w: u32, h: u32) -> Image {
    let mut state = 0x2545_f491_u32;
    let samples = (0..w * h * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect();
    Image::from_u8(w, h, ColorType::Rgb, samples).unwrap()
}

#[test]
fn tiny_images_complete_without_error() {
    for (w, h) in [(1, 1), (2, 2), (3, 1), (8, 8)] {
        let report = search(&noise(w, h), 70.0);
        // At most 64 pixels, so at most 64 colours: some palette is exact.
        assert!(report.reached, "{w}x{h}: {report:?}");
    }
}

#[test]
fn a_flat_image_walks_down_to_a_few_colours() {
    let flat = Image::from_u8(32, 32, ColorType::Rgb, vec![90; 32 * 32 * 3]).unwrap();
    let report = search(&flat, 70.0);
    assert!(report.reached, "{report:?}");
    assert!(report.value <= 3.0, "{report:?}");
    assert!(report.trials.windows(2).all(|w| w[1].value < w[0].value));
}

#[test]
fn pure_noise_completes_and_takes_a_large_palette() {
    // 4096 pixels of nearly as many colours. Dithered, noise hides the
    // palette's error well, so 70 is in reach, but only with many colours.
    let report = search(&noise(64, 64), 70.0);
    assert!(report.reached || report.capped, "{report:?}");
    assert!(report.value >= 64.0, "{report:?}");
}

#[test]
fn a_target_out_of_reach_at_256_is_capped_not_an_error() {
    // A smooth two-axis gradient: in reach of a palette at 40, not at 99.
    let mut samples = Vec::with_capacity(64 * 64 * 3);
    for y in 0..64u32 {
        for x in 0..64u32 {
            samples.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8]);
        }
    }
    let gradient = Image::from_u8(64, 64, ColorType::Rgb, samples).unwrap();
    let report = search(&gradient, 99.0);
    assert!(!report.reached && report.capped, "{report:?}");
    assert_eq!(report.value, 256.0);
    let easy = search(&gradient, 40.0);
    assert!(easy.reached, "{easy:?}");
    assert!(easy.value < 256.0, "{easy:?}");
}
