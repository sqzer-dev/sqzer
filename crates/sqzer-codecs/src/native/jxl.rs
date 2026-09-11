//! JPEG XL encoding via `libjxl` 0.12, through `gamut-jxl` (MIT OR
//! Apache-2.0) over `gamut-jxl-sys`, whose FFI declarations are written by
//! hand so no `bindgen` runs at build time. `jpegxl-src` (BSD-3-Clause)
//! vendors libjxl with highway, brotli and skcms and builds it with cmake.
//! Decoding stays with `jxl-oxide` in the portable tier.
//!
//! No parallel runner is installed, so libjxl encodes on the calling
//! thread (ADR-0001 D3).

use gamut_core::{
    Dimensions, EncodeImage, Gray8, Gray16, GrayAlpha8, GrayAlpha16, ImageRef, Pixel, Rgb8, Rgb16,
    Rgba8, Rgba16,
};
use gamut_jxl::{ColorSpec, Container, Distance, Effort, JxlEncoder};
use sqzer_core::codec::{CodecOption, Encoder, EncoderCaps, Format, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::{EncodeParams, Resolved, Subsampling};
use sqzer_core::{Error, Result};

use crate::opts::{parse_bool, unknown};

/// The smallest lossy distance handed to libjxl. Quality 100 lands here;
/// distance 0 would be lossless, which is a separate mode, not a point on
/// the quality scale.
const MIN_DISTANCE: f32 = 0.01;

/// JPEG XL encoder over `libjxl`. Lossy (`VarDCT`) and lossless (modular),
/// 8 and 16-bit, gray and alpha, an ICC profile embedded in the
/// codestream.
///
/// A lossy encode is XYB-coded: the profile says how to read the input,
/// and decoders render the result to sRGB, so what comes back carries no
/// profile. A lossless encode keeps the samples and the profile as given.
///
/// Quality `0..=100` maps onto a Butteraugli distance the way `cjxl`'s
/// `--quality` does: 90 is distance 1.0, "visually lossless", 100 is the
/// smallest lossy distance, and the scale steepens below 30. Effort
/// `0..=10` maps onto libjxl's `1..=10` as `effort + 1`, so the default
/// effort, 6, lands on libjxl's own default, 7.
///
/// `VarDCT` has no chroma subsampling knob in this binding, so
/// `Subsampling::S420` and `S422` are refused rather than ignored.
///
/// Options, all `jxl:` prefixed:
/// - `container`: wrap the codestream in the ISO BMFF `.jxl` container
///   (default `false`). The bare codestream is smaller; the container is
///   what metadata boxes need.
#[derive(Debug, Clone, Copy, Default)]
pub struct LibjxlEncoder;

static ENCODER_CAPS: EncoderCaps = EncoderCaps {
    format: Format::Jxl,
    name: "gamut-jxl",
    lossy: true,
    lossless: true,
    alpha: true,
    animation: false,
    bit_depth: &[8, 16],
    hdr: false,
    quality_range: 0.0..=100.0,
    effort_range: 1..=10,
    tier: Tier::Native,
    options: &[CodecOption {
        key: "container",
        default: "false",
        help: "wrap the codestream in the ISO BMFF `.jxl` container",
    }],
};

impl Encoder for LibjxlEncoder {
    fn caps(&self) -> &EncoderCaps {
        &ENCODER_CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>> {
        let resolved = params.resolved()?;
        let mut container = false;
        for (key, value) in params.codec_opts("jxl") {
            match key {
                "container" => container = parse_bool("jxl", key, value)?,
                _ => return Err(unknown("jxl", key)),
            }
        }
        let mut encoder = match resolved {
            Resolved::Quality(q) => {
                match params.subsampling {
                    Subsampling::Auto | Subsampling::S444 => {}
                    Subsampling::S422 | Subsampling::S420 => {
                        return Err(unsupported("chroma subsampling"));
                    }
                }
                JxlEncoder::lossy(map_quality(q)?)
            }
            Resolved::Lossless => JxlEncoder::lossless(),
        }
        .with_effort(map_effort(params.effort))
        .with_container(if container {
            Container::IsoBmff
        } else {
            Container::Codestream
        });
        if let Some(icc) = img.icc() {
            encoder = encoder.with_color(ColorSpec::Icc(icc.to_vec()));
        }

        let dims = Dimensions {
            width: img.width(),
            height: img.height(),
        };
        match (img.samples(), img.color()) {
            (Samples::U8(v), ColorType::Gray) => encode_as::<Gray8>(&encoder, v, dims),
            (Samples::U8(v), ColorType::GrayAlpha) => encode_as::<GrayAlpha8>(&encoder, v, dims),
            (Samples::U8(v), ColorType::Rgb) => encode_as::<Rgb8>(&encoder, v, dims),
            (Samples::U8(v), ColorType::Rgba) => encode_as::<Rgba8>(&encoder, v, dims),
            (Samples::U16(v), ColorType::Gray) => encode_as::<Gray16>(&encoder, v, dims),
            (Samples::U16(v), ColorType::GrayAlpha) => encode_as::<GrayAlpha16>(&encoder, v, dims),
            (Samples::U16(v), ColorType::Rgb) => encode_as::<Rgb16>(&encoder, v, dims),
            (Samples::U16(v), ColorType::Rgba) => encode_as::<Rgba16>(&encoder, v, dims),
            (Samples::F32(_), _) => Err(unsupported("float (HDR) samples")),
        }
    }
}

/// Hand one sample layout to the encoder.
fn encode_as<P: Pixel>(
    encoder: &JxlEncoder,
    data: &[P::Sample],
    dims: Dimensions,
) -> Result<Vec<u8>>
where
    JxlEncoder: EncodeImage<P>,
{
    let image = ImageRef::<P>::new(data, dims).map_err(codec_err)?;
    encoder.encode_to_vec(image).map_err(codec_err)
}

/// Abstract `0..=100` to a Butteraugli distance, `cjxl`'s own mapping.
fn map_quality(q: f32) -> Result<Distance> {
    if q.is_nan() {
        return Err(Error::InvalidParams("quality is NaN".into()));
    }
    let q = q.clamp(0.0, 100.0);
    let distance = if q >= 100.0 {
        MIN_DISTANCE
    } else if q >= 30.0 {
        0.1 + (100.0 - q) * 0.09
    } else {
        53.0 / 3000.0 * q * q - 23.0 / 20.0 * q + 25.0
    };
    Distance::new(distance.clamp(MIN_DISTANCE, 25.0)).map_err(codec_err)
}

/// Effort `0..=10` to libjxl effort `1..=10`. The default effort, 6,
/// lands on libjxl's default, 7.
fn map_effort(effort: u8) -> Effort {
    Effort::from_level(effort.saturating_add(1).min(10)).expect("1..=10")
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported {
        format: Format::Jxl,
        what: what.into(),
    }
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distance(q: f32) -> f32 {
        map_quality(q).unwrap().get()
    }

    #[test]
    fn quality_follows_cjxl() {
        assert!((distance(90.0) - 1.0).abs() < 1e-6);
        assert!((distance(75.0) - 2.35).abs() < 1e-6);
        // The two branches meet at 30.
        assert!((distance(30.0) - 6.4).abs() < 1e-5);
        assert!((distance(0.0) - 25.0).abs() < 1e-6);
        assert!((distance(100.0) - MIN_DISTANCE).abs() < 1e-6);
        assert!((distance(250.0) - MIN_DISTANCE).abs() < 1e-6);
        assert!(map_quality(f32::NAN).is_err());
    }

    #[test]
    fn effort_shifts_by_one() {
        assert_eq!(map_effort(0).level(), 1);
        assert_eq!(map_effort(6).level(), 7);
        assert_eq!(map_effort(9).level(), 10);
        assert_eq!(map_effort(10).level(), 10);
        assert_eq!(map_effort(255).level(), 10);
    }
}
