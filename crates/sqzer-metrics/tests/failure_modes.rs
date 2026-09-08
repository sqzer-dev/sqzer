//! The search against the real metric on the inputs ADR-0001 D4 names as
//! failure modes: tiny images, flat images, pure noise, and a target the
//! encoder cannot reach at its ceiling.
//!
//! No real codec is involved. `Quantiser` is a fake lossy backend that
//! rounds samples to a step size derived from quality, so its score rises
//! with quality by construction and the tests exercise the search rather
//! than an encoder's quirks.

// Synthetic pixel data: the truncating casts are the point. Qualities are
// whole numbers, so exact float comparison is the intent.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::many_single_char_names
)]

use sqzer_core::codec::{Decoder, DecoderCaps, Encoder, EncoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image};
use sqzer_core::metric::Metric;
use sqzer_core::params::{DecodeOpts, EncodeParams, Resolved};
use sqzer_core::{Error, Registry, Result};
use sqzer_metrics::{Found, Reference, Search, Ssimulacra2};

const MAGIC: &[u8; 4] = b"QNTZ";

/// Fake lossy encoder: samples rounded, relative to the image's darkest
/// sample, to a multiple of a step that shrinks as quality rises. The
/// offset makes a flat image survive exactly at every quality, as it would
/// through a real codec. `min_step` bounds how good it can ever get.
struct Quantiser {
    min_step: u8,
}

static ENC_CAPS: EncoderCaps = EncoderCaps {
    format: Format::Tiff,
    name: "quantiser",
    lossy: true,
    lossless: false,
    alpha: true,
    animation: false,
    bit_depth: &[8],
    hdr: false,
    quality_range: 1.0..=100.0,
    effort_range: 0..=0,
    tier: Tier::Portable,
    options: &[],
};

impl Quantiser {
    fn step(&self, quality: f32) -> u8 {
        let step = 1.0 + (100.0 - quality) / 100.0 * 63.0;
        (step.round() as u8).max(self.min_step)
    }
}

impl Encoder for Quantiser {
    fn caps(&self) -> &EncoderCaps {
        &ENC_CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>> {
        let Resolved::Quality(q) = params.resolved()? else {
            panic!("lossless is not claimed");
        };
        let step = u16::from(self.step(q));
        let samples = img.samples().as_u8().expect("8-bit test input");
        let base = u16::from(samples.iter().copied().min().unwrap_or(0));
        let mut out = Vec::with_capacity(samples.len() + 13);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&img.width().to_le_bytes());
        out.extend_from_slice(&img.height().to_le_bytes());
        out.push(img.channels() as u8);
        out.extend(samples.iter().map(|&s| {
            let v = base + (u16::from(s) - base + step / 2) / step * step;
            v.min(255) as u8
        }));
        Ok(out)
    }
}

struct Dequantiser;

static DEC_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Tiff,
    name: "dequantiser",
    animation: false,
    tier: Tier::Portable,
};

impl Decoder for Dequantiser {
    fn caps(&self) -> &DecoderCaps {
        &DEC_CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        bytes.starts_with(MAGIC).then_some(FormatInfo {
            format: Format::Tiff,
            animated: false,
        })
    }

    fn dimensions(&self, _: &[u8]) -> Option<(u32, u32)> {
        None
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let w = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let h = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        opts.check_pixels(w, h)?;
        let color = match bytes[12] {
            1 => ColorType::Gray,
            3 => ColorType::Rgb,
            4 => ColorType::Rgba,
            _ => unreachable!(),
        };
        Image::from_u8(w, h, color, bytes[13..].to_vec())
    }
}

fn registry() -> Registry {
    let mut reg = Registry::new();
    reg.register_decoder(Dequantiser);
    reg
}

fn gradient(w: u32, h: u32) -> Image {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / (w - 1).max(1)) as u8;
            let g = (y * 255 / (h - 1).max(1)) as u8;
            let b = if x < w / 2 { 40 } else { 220 };
            v.extend([r, g, b]);
        }
    }
    Image::from_u8(w, h, ColorType::Rgb, v).unwrap()
}

fn flat(w: u32, h: u32) -> Image {
    Image::from_u8(w, h, ColorType::Rgb, vec![137; (w * h * 3) as usize]).unwrap()
}

/// Deterministic white noise from a linear congruential generator.
fn noise(w: u32, h: u32) -> Image {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let v = (0..w * h * 3)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 56) as u8
        })
        .collect();
    Image::from_u8(w, h, ColorType::Rgb, v).unwrap()
}

/// Run the default search over the quantiser for `img`.
fn search(img: &Image, min_step: u8, search: &Search) -> Found<Vec<u8>> {
    let mut reference = Reference::new(img).unwrap();
    search
        .encode(
            &Quantiser { min_step },
            img,
            &EncodeParams::default(),
            &registry(),
            |candidate| reference.score(candidate),
        )
        .unwrap()
}

/// The report describes the output it came with.
fn assert_consistent(img: &Image, found: &Found<Vec<u8>>) {
    let r = &found.report;
    assert!(r.iterations >= 1 && r.iterations <= 6, "{r:?}");
    assert_eq!(r.trials.len(), usize::from(r.iterations));
    assert!(r.trials.iter().all(|t| (1.0..=100.0).contains(&t.quality)));
    let decoded = registry()
        .decode(&found.output, &DecodeOpts::default())
        .unwrap()
        .image;
    let rescored = Ssimulacra2.score(img, &decoded).unwrap();
    assert!(
        (rescored - r.score).abs() < 1e-3,
        "output scores {rescored}, report says {}",
        r.score
    );
    assert_eq!(r.reached, r.score >= 69.0, "{r:?}");
    if r.capped {
        assert!(!r.reached);
        assert!(r.trials.iter().any(|t| t.quality == 100.0));
    }
}

#[test]
fn normal_image_reaches_the_target_within_budget() {
    let img = gradient(48, 32);
    let found = search(&img, 1, &Search::new(70.0));
    let r = &found.report;
    assert_consistent(&img, &found);
    assert!(r.reached, "{r:?}");
    assert!(!r.capped);
    assert!(
        r.score < 100.0,
        "a lossy pick, not the lossless ceiling: {r:?}"
    );
}

#[test]
fn flat_image_walks_down_to_the_floor() {
    let img = flat(48, 32);
    let found = search(&img, 1, &Search::new(70.0));
    let r = &found.report;
    assert_consistent(&img, &found);
    assert!(r.reached);
    assert!(r.quality <= 3.0, "{r:?}");
    assert!(r.trials.iter().all(|t| t.score > 99.99), "{r:?}");
}

#[test]
fn tiny_images_complete_without_error() {
    for (w, h) in [(1, 1), (2, 2), (3, 5), (7, 7)] {
        let img = gradient(w, h);
        let found = search(&img, 1, &Search::new(70.0));
        assert_consistent(&img, &found);
        let decoded = registry()
            .decode(&found.output, &DecodeOpts::default())
            .unwrap()
            .image;
        assert_eq!((decoded.width(), decoded.height()), (w, h));
    }
}

/// The same target lands on different qualities for different content,
/// which is the reason the perceptual target exists. The direction is the
/// metric's business: SSIMULACRA2 forgives quantised noise, where the
/// error hides in the texture, more than it forgives banding in a smooth
/// gradient, so noise needs the lower quality of the two here.
#[test]
fn pure_noise_reaches_and_lands_elsewhere_than_a_gradient() {
    let smooth = gradient(48, 32);
    let noisy = noise(48, 32);
    let a = search(&smooth, 1, &Search::new(70.0));
    let b = search(&noisy, 1, &Search::new(70.0));
    assert_consistent(&smooth, &a);
    assert_consistent(&noisy, &b);
    assert!(b.report.reached, "{:?}", b.report);
    assert!(!b.report.capped);
    assert!(
        (b.report.quality - a.report.quality).abs() >= 5.0,
        "noise {:?} vs gradient {:?}",
        b.report,
        a.report
    );
}

#[test]
fn unreachable_at_the_ceiling_is_reported_not_errored() {
    let img = noise(48, 32);
    let found = search(&img, 48, &Search::new(70.0));
    let r = &found.report;
    assert_consistent(&img, &found);
    assert!(!r.reached, "{r:?}");
    assert!(r.capped, "{r:?}");
    assert_eq!(r.quality, 100.0);
    assert!(!found.output.is_empty());
}

#[test]
fn a_target_above_the_scale_is_capped_too() {
    let img = gradient(48, 32);
    let found = search(&img, 1, &Search::new(150.0));
    assert!(found.report.capped, "{:?}", found.report);
    assert_eq!(found.report.quality, 100.0);
}

#[test]
fn one_shot_metric_and_reference_agree_on_the_pick() {
    let img = gradient(48, 32);
    let with_reference = search(&img, 1, &Search::new(70.0));
    let one_shot = Search::new(70.0)
        .encode(
            &Quantiser { min_step: 1 },
            &img,
            &EncodeParams::default(),
            &registry(),
            |candidate| Ssimulacra2.score(&img, candidate),
        )
        .unwrap();
    assert_eq!(with_reference.report.quality, one_shot.report.quality);
    assert_eq!(with_reference.output, one_shot.output);
}

#[test]
fn undecodable_output_is_unsupported_not_unknown() {
    let img = gradient(16, 16);
    let err = Search::new(70.0)
        .encode(
            &Quantiser { min_step: 1 },
            &img,
            &EncodeParams::default(),
            &Registry::new(),
            |candidate| Ssimulacra2.score(&img, candidate),
        )
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Unsupported {
                format: Format::Tiff,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn print_reports_for_calibration() {
    for (name, img, step) in [
        ("gradient", gradient(48, 32), 1),
        ("flat", flat(48, 32), 1),
        ("noise", noise(48, 32), 1),
        ("noise-capped", noise(48, 32), 48),
        ("1x1", gradient(1, 1), 1),
        ("2x2", gradient(2, 2), 1),
        ("3x5", gradient(3, 5), 1),
        ("7x7", gradient(7, 7), 1),
    ] {
        let found = search(&img, step, &Search::new(70.0));
        eprintln!("{name}: {:?}", found.report);
    }
}
