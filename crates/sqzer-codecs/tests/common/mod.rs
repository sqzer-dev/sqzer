//! Shared helpers for the integration tests: the synthetic pattern every
//! fixture under `tests/fixtures` encodes, and small assertions over it.
//!
//! The pattern is generated, never read from disk, so lossless decoders can
//! be checked for exact equality and lossy ones for a bounded error.
//! `tests/fixtures/README.md` says how the files were produced.

#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use std::path::PathBuf;

use sqzer_core::image::{ColorType, Image, SampleFormat, Samples};
use sqzer_core::params::{EncodeParams, Target};

pub const W: u32 = 48;
pub const H: u32 = 32;

/// A smooth gradient with a hard edge, so subsampling and DCT both matter.
pub fn test_image(color: ColorType) -> Image {
    let ch = color.channels();
    let mut samples = Vec::with_capacity(W as usize * H as usize * ch);
    for y in 0..H {
        for x in 0..W {
            let r = (x * 255 / (W - 1)) as u8;
            let g = (y * 255 / (H - 1)) as u8;
            let b = if x < W / 2 { 40 } else { 220 };
            let a = if y % 2 == 0 { 255 } else { 128 };
            let gray = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
            match color {
                ColorType::Gray => samples.push(gray),
                ColorType::GrayAlpha => samples.extend([gray, a]),
                ColorType::Rgb => samples.extend([r, g, b]),
                ColorType::Rgba => samples.extend([r, g, b, a]),
            }
        }
    }
    Image::from_u8(W, H, color, samples).unwrap()
}

/// The pattern widened to 16 bits by replication, as a 16-bit source would
/// store it.
pub fn test_image_u16(color: ColorType) -> Image {
    let img = test_image(color);
    let wide: Vec<u16> = img
        .samples()
        .as_u8()
        .unwrap()
        .iter()
        .map(|&s| u16::from(s) * 257)
        .collect();
    Image::from_u16(W, H, color, wide).unwrap()
}

pub fn quality(q: f32) -> EncodeParams {
    EncodeParams {
        target: Target::Quality(q),
        ..Default::default()
    }
}

pub trait IntoParams {
    fn into_params(self) -> EncodeParams;
}

impl IntoParams for Target {
    fn into_params(self) -> EncodeParams {
        EncodeParams {
            target: self,
            ..Default::default()
        }
    }
}

/// Bytes of a file in `tests/fixtures` at the workspace root.
pub fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Mean absolute error between two images of the same shape, in the units
/// of their sample type. Panics on a shape mismatch, which is its own bug.
pub fn mae(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.width(), a.height()), (b.width(), b.height()), "size");
    assert_eq!(a.color(), b.color(), "layout");
    assert_eq!(a.sample_format(), b.sample_format(), "sample format");
    let sum: f64 = match (a.samples(), b.samples()) {
        (Samples::U8(x), Samples::U8(y)) => x
            .iter()
            .zip(y)
            .map(|(&p, &q)| f64::from(p.abs_diff(q)))
            .sum(),
        (Samples::U16(x), Samples::U16(y)) => x
            .iter()
            .zip(y)
            .map(|(&p, &q)| f64::from(p.abs_diff(q)))
            .sum(),
        (Samples::F32(x), Samples::F32(y)) => x
            .iter()
            .zip(y)
            .map(|(&p, &q)| f64::from((p - q).abs()))
            .sum(),
        _ => unreachable!("checked above"),
    };
    sum / a.samples().len() as f64
}

/// Mean absolute error over one channel only, in the sample type's units.
pub fn mae_channel(a: &Image, b: &Image, channel: usize) -> f64 {
    let ch = a.channels();
    let diffs: Vec<f64> = match (a.samples(), b.samples()) {
        (Samples::U8(x), Samples::U8(y)) => x
            .iter()
            .zip(y)
            .map(|(&p, &q)| f64::from(p.abs_diff(q)))
            .collect(),
        (Samples::U16(x), Samples::U16(y)) => x
            .iter()
            .zip(y)
            .map(|(&p, &q)| f64::from(p.abs_diff(q)))
            .collect(),
        _ => panic!("sample formats differ"),
    };
    let sum: f64 = diffs.iter().skip(channel).step_by(ch).sum();
    sum / (diffs.len() / ch) as f64
}

/// Assert an image is the pattern within `limit` mean absolute error.
pub fn assert_close(decoded: &Image, expected: &Image, limit: f64, what: &str) {
    assert_eq!(decoded.sample_format(), expected.sample_format(), "{what}");
    let err = mae(decoded, expected);
    assert!(
        err <= limit,
        "{what}: mean absolute error {err:.2} > {limit}"
    );
}

pub fn is_icc(bytes: &[u8]) -> bool {
    bytes.len() > 128 && &bytes[36..40] == b"acsp"
}

pub fn sample_format(img: &Image) -> SampleFormat {
    img.sample_format()
}
