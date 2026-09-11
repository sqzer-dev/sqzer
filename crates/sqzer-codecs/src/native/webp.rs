//! Lossy and lossless WebP via `libwebp`, through `webpx` (MIT OR
//! Apache-2.0) over `libwebp-sys` (MIT). The library is vendored and built
//! by `cc`; nothing is linked from the system. Takes over the WebP format
//! from the portable lossless writer when `native-webp` is on; decoding
//! stays with `image-webp`.

use sqzer_core::codec::{CodecOption, Encoder, EncoderCaps, Format, Tier};
use sqzer_core::image::{ColorType, Image};
use sqzer_core::params::{EncodeParams, Resolved, Subsampling};
use sqzer_core::{Error, Result};
use webpx::Unstoppable;

use crate::layout::widen_gray;
use crate::opts::{parse_bool, parse_percent, unknown};

/// WebP encoder over `libwebp`: lossy (VP8) and lossless (VP8L), 8-bit,
/// alpha, ICC kept in an `ICCP` chunk. Gray input is widened to RGB, WebP
/// has no gray layout; 16-bit input is rounded to 8 bits.
///
/// Quality maps one to one onto `libwebp`'s `0..=100`. Effort `0..=10`
/// maps onto `libwebp`'s method `0..=6`; the default effort, 6, lands on
/// method 4, `cwebp`'s default and what the seed table was calibrated at.
///
/// Lossy VP8 is always 4:2:0, so `Subsampling::S444` and `S422` are
/// refused rather than ignored. Lossless output is exact, including the
/// colour of fully transparent pixels.
///
/// Options, all `webp:` prefixed:
/// - `alpha_quality`: `0..=100`, default `100`, which stores alpha
///   losslessly.
/// - `sharp_yuv`: slower, more accurate RGB to YUV conversion (default
///   `false`).
#[derive(Debug, Clone, Copy, Default)]
pub struct LibwebpEncoder;

static ENCODER_CAPS: EncoderCaps = EncoderCaps {
    format: Format::WebP,
    name: "webpx",
    lossy: true,
    lossless: true,
    alpha: true,
    animation: false,
    bit_depth: &[8],
    hdr: false,
    quality_range: 0.0..=100.0,
    effort_range: 0..=6,
    tier: Tier::Native,
    options: &[
        CodecOption {
            key: "alpha_quality",
            default: "100",
            help: "alpha plane quality `0..=100`; `100` stores alpha losslessly",
        },
        CodecOption {
            key: "sharp_yuv",
            default: "false",
            help: "slower, more accurate RGB to YUV conversion for lossy output",
        },
    ],
};

impl Encoder for LibwebpEncoder {
    fn caps(&self) -> &EncoderCaps {
        &ENCODER_CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>> {
        let resolved = params.resolved()?;
        let mut alpha_quality = 100;
        let mut sharp_yuv = false;
        for (key, value) in params.codec_opts("webp") {
            match key {
                "alpha_quality" => alpha_quality = parse_percent("webp", key, value)?,
                "sharp_yuv" => sharp_yuv = parse_bool("webp", key, value)?,
                _ => return Err(unknown("webp", key)),
            }
        }
        if let Resolved::Quality(_) = resolved {
            match params.subsampling {
                Subsampling::Auto | Subsampling::S420 => {}
                Subsampling::S444 | Subsampling::S422 => {
                    return Err(Error::Unsupported {
                        format: Format::WebP,
                        what: "chroma subsampling other than 4:2:0".into(),
                    });
                }
            }
        }

        let img = img.to_u8(Format::WebP)?;
        let samples = img
            .samples()
            .as_u8()
            .ok_or_else(|| Error::Codec("expected 8-bit samples after conversion".into()))?;
        let (w, h) = (img.width(), img.height());
        let widened;
        let (data, alpha) = match img.color() {
            ColorType::Rgb => (samples, false),
            ColorType::Rgba => (samples, true),
            ColorType::Gray => {
                widened = widen_gray(samples, 1);
                (widened.as_slice(), false)
            }
            ColorType::GrayAlpha => {
                widened = widen_gray(samples, 2);
                (widened.as_slice(), true)
            }
        };

        let mut encoder = if alpha {
            webpx::Encoder::new_rgba(data, w, h)
        } else {
            webpx::Encoder::new_rgb(data, w, h)
        }
        .method(map_effort(params.effort))
        .alpha_quality(alpha_quality)
        .sharp_yuv(sharp_yuv);
        encoder = match resolved {
            Resolved::Quality(q) => encoder.quality(map_quality(q)?),
            Resolved::Lossless => encoder.lossless(true).exact(true),
        };
        if let Some(icc) = img.icc() {
            encoder = encoder.icc_profile(icc);
        }
        encoder
            .encode(Unstoppable)
            .map_err(|e| Error::Codec(e.to_string()))
    }
}

/// Abstract `0..=100` to `libwebp`'s `0..=100`. The scales agree.
fn map_quality(q: f32) -> Result<f32> {
    if q.is_nan() {
        return Err(Error::InvalidParams("quality is NaN".into()));
    }
    Ok(q.clamp(0.0, 100.0))
}

/// Effort `0..=10` to `libwebp` method `0..=6`, rounded to nearest, so the
/// default effort, 6, is method 4.
fn map_effort(effort: u8) -> u8 {
    let scaled = (u16::from(effort.min(10)) * 6 + 5) / 10;
    u8::try_from(scaled).expect("at most 6")
}

#[cfg(test)]
// Exact float comparison is the point: the mapping must not round.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn quality_mapping_clamps() {
        assert_eq!(map_quality(-3.0).unwrap(), 0.0);
        assert_eq!(map_quality(74.6).unwrap(), 74.6);
        assert_eq!(map_quality(250.0).unwrap(), 100.0);
        assert!(map_quality(f32::NAN).is_err());
    }

    #[test]
    fn effort_maps_onto_method() {
        assert_eq!(map_effort(0), 0);
        assert_eq!(map_effort(3), 2);
        assert_eq!(map_effort(6), 4);
        assert_eq!(map_effort(8), 5);
        assert_eq!(map_effort(10), 6);
        assert_eq!(map_effort(200), 6);
    }
}
