//! AVIF. Encoding via `ravif` (BSD-3-Clause) over `rav1e`, on every
//! target. Decoding via `avif-parse`, `re_rav1d` and `yuv`, desktop only:
//! see [`AvifDecoder`] for why.

#[cfg(not(target_arch = "wasm32"))]
mod decode;
#[cfg(not(target_arch = "wasm32"))]
pub use decode::AvifDecoder;

use ravif::{AlphaColorMode, BitDepth, ColorModel, Img};
use rgb::FromSlice;
use sqzer_core::codec::{CodecOption, Encoder, EncoderCaps, Format, Tier};
use sqzer_core::image::{ColorType, Image};
use sqzer_core::params::{EncodeParams, Resolved, Subsampling};
use sqzer_core::{Error, Result};

use crate::layout::widen_gray;
use crate::opts::unknown;

/// AVIF encoder over `rav1e`. Lossy, 8-bit input, 4:4:4, alpha as a
/// separate item. The AV1 payload is 10-bit by default, which `rav1e`
/// compresses better even from 8-bit sources; decoders return it as
/// `u16`. `avif:bit_depth=8` keeps the payload 8-bit.
///
/// What this backend refuses rather than approximates:
/// - `Target::Lossless`. `ravif` has no lossless mode.
/// - `Subsampling::S420` and `S422`. `ravif` always writes 4:4:4.
/// - An image carrying an ICC profile. `ravif` writes an sRGB `colr` box
///   and cannot embed a profile, so until the pipeline converts to sRGB
///   and drops it (ADR-0001 D3) such an image is refused, not silently
///   re-tagged.
///
/// Gray input is encoded as RGB; AVIF has a monochrome mode but `ravif`
/// does not expose it.
///
/// Options, all `avif:` prefixed:
/// - `alpha_quality`: `1..=100`, or `auto` (default) to follow the colour
///   quality.
/// - `bit_depth`: `8`, `10` or `auto` (default `auto`, which is 10).
/// - `color_model`: `ycbcr` (default) or `rgb`; `rgb` stores GBR planes
///   with an identity matrix, larger but exact in colour.
#[derive(Debug, Clone, Copy, Default)]
pub struct RavifEncoder;

static ENCODER_CAPS: EncoderCaps = EncoderCaps {
    format: Format::Avif,
    name: "ravif",
    lossy: true,
    lossless: false,
    alpha: true,
    animation: false,
    bit_depth: &[8],
    hdr: false,
    quality_range: 1.0..=100.0,
    effort_range: 0..=10,
    tier: Tier::Portable,
    options: &[
        CodecOption {
            key: "alpha_quality",
            default: "auto",
            help: "alpha plane quality `1..=100`; `auto` follows the colour quality",
        },
        CodecOption {
            key: "bit_depth",
            default: "auto",
            help: "AV1 payload depth, `8`, `10` or `auto` (10)",
        },
        CodecOption {
            key: "color_model",
            default: "ycbcr",
            help: "`ycbcr` or `rgb`; `rgb` is larger but exact in colour",
        },
    ],
};

impl Encoder for RavifEncoder {
    fn caps(&self) -> &EncoderCaps {
        &ENCODER_CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>> {
        let quality = match params.resolved()? {
            Resolved::Quality(q) => map_quality(q)?,
            Resolved::Lossless => return Err(unsupported("lossless output")),
        };
        match params.subsampling {
            Subsampling::Auto | Subsampling::S444 => {}
            Subsampling::S422 | Subsampling::S420 => {
                return Err(unsupported("chroma subsampling"));
            }
        }
        if img.icc().is_some() {
            return Err(unsupported("an embedded ICC profile"));
        }

        // Single-threaded until the thread budget lands (ADR-0001 D3).
        let mut encoder = ravif::Encoder::new()
            .with_quality(quality)
            .with_alpha_quality(quality)
            .with_speed(map_effort(params.effort))
            .with_alpha_color_mode(AlphaColorMode::UnassociatedClean)
            .with_num_threads(Some(1));

        for (key, value) in params.codec_opts("avif") {
            encoder = match key {
                "alpha_quality" if value == "auto" => encoder,
                "alpha_quality" => encoder.with_alpha_quality(parse_quality(key, value)?),
                "bit_depth" => encoder.with_bit_depth(parse_bit_depth(value)?),
                "color_model" => encoder.with_internal_color_model(parse_color_model(value)?),
                _ => return Err(unknown("avif", key)),
            };
        }

        let img = img.to_u8(Format::Avif)?;
        let samples = img
            .samples()
            .as_u8()
            .ok_or_else(|| Error::Codec("expected 8-bit samples after conversion".into()))?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        let out = match img.color() {
            ColorType::Rgb => encoder.encode_rgb(Img::new(samples.as_rgb(), w, h)),
            ColorType::Rgba => encoder.encode_rgba(Img::new(samples.as_rgba(), w, h)),
            ColorType::Gray => {
                let rgb = widen_gray(samples, 1);
                encoder.encode_rgb(Img::new(rgb.as_rgb(), w, h))
            }
            ColorType::GrayAlpha => {
                let rgba = widen_gray(samples, 2);
                encoder.encode_rgba(Img::new(rgba.as_rgba(), w, h))
            }
        };
        out.map(|encoded| encoded.avif_file)
            .map_err(|e| Error::Codec(e.to_string()))
    }
}

/// Abstract `0..=100` to `ravif`'s `1..=100`. The scales agree; only zero
/// has no meaning on the AV1 side.
fn map_quality(q: f32) -> Result<f32> {
    if q.is_nan() {
        return Err(Error::InvalidParams("quality is NaN".into()));
    }
    Ok(q.clamp(1.0, 100.0))
}

/// Effort `0..=10` to `rav1e` speed `10..=1`. The default effort, 6, lands
/// on speed 4, `ravif`'s own recommendation.
fn map_effort(effort: u8) -> u8 {
    10u8.saturating_sub(effort).clamp(1, 10)
}

fn parse_quality(key: &str, value: &str) -> Result<f32> {
    value
        .parse::<f32>()
        .ok()
        .filter(|q| (1.0..=100.0).contains(q))
        .ok_or_else(|| {
            Error::InvalidParams(format!(
                "avif:{key} expects a number 1..=100, got `{value}`"
            ))
        })
}

fn parse_bit_depth(value: &str) -> Result<BitDepth> {
    match value {
        "8" => Ok(BitDepth::Eight),
        "10" => Ok(BitDepth::Ten),
        "auto" => Ok(BitDepth::Auto),
        _ => Err(Error::InvalidParams(format!(
            "avif:bit_depth expects 8, 10 or auto, got `{value}`"
        ))),
    }
}

fn parse_color_model(value: &str) -> Result<ColorModel> {
    match value {
        "ycbcr" => Ok(ColorModel::YCbCr),
        "rgb" => Ok(ColorModel::RGB),
        _ => Err(Error::InvalidParams(format!(
            "avif:color_model expects ycbcr or rgb, got `{value}`"
        ))),
    }
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported {
        format: Format::Avif,
        what: what.into(),
    }
}

#[cfg(test)]
// Exact float comparison is the point: the mapping must not round.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn quality_mapping_clamps() {
        assert_eq!(map_quality(0.0).unwrap(), 1.0);
        assert_eq!(map_quality(74.6).unwrap(), 74.6);
        assert_eq!(map_quality(250.0).unwrap(), 100.0);
        assert!(map_quality(f32::NAN).is_err());
    }

    #[test]
    fn effort_inverts_to_speed() {
        assert_eq!(map_effort(0), 10);
        assert_eq!(map_effort(6), 4);
        assert_eq!(map_effort(9), 1);
        assert_eq!(map_effort(10), 1);
    }

    #[test]
    fn options_are_validated() {
        assert!(parse_quality("alpha_quality", "0").is_err());
        assert!(parse_quality("alpha_quality", "x").is_err());
        assert_eq!(parse_quality("alpha_quality", "55.5").unwrap(), 55.5);
        assert!(parse_bit_depth("12").is_err());
        assert!(parse_color_model("yuv").is_err());
    }
}
