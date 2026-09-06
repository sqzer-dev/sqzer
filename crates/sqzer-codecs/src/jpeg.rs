//! JPEG via `mozjpeg-rs` (BSD-3): a pure-Rust port of mozjpeg with
//! byte-identical baseline and progressive output and trellis quantisation.
//! Encoder only; JPEG decoding is `zune-jpeg` (ADR-0001 item 3).

use sqzer_core::codec::{Encoder, EncoderCaps, Format, Tier};
use sqzer_core::image::{ColorType, Image};
use sqzer_core::params::{EncodeParams, Resolved, Subsampling};
use sqzer_core::{Error, Result};

/// Above this abstract quality `Subsampling::Auto` stops subsampling chroma.
const AUTO_444_THRESHOLD: u8 = 90;

/// mozjpeg encoder.
#[derive(Debug, Clone, Copy, Default)]
pub struct MozjpegEncoder;

static CAPS: EncoderCaps = EncoderCaps {
    format: Format::Jpeg,
    lossy: true,
    lossless: false,
    alpha: false,
    animation: false,
    bit_depth: &[8],
    hdr: false,
    quality_range: 1.0..=100.0,
    effort_range: 0..=10,
    tier: Tier::Portable,
};

impl Encoder for MozjpegEncoder {
    fn caps(&self) -> &EncoderCaps {
        &CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>> {
        let quality = match params.resolved()? {
            Resolved::Quality(q) => map_quality(q),
            Resolved::Lossless => {
                return Err(Error::Unsupported {
                    format: Format::Jpeg,
                    what: "lossless output".into(),
                });
            }
        };

        let mut encoder = mozjpeg_rs::Encoder::new(map_effort(params.effort))
            .quality(quality)
            .subsampling(map_subsampling(params.subsampling, quality));

        for (key, value) in params.codec_opts("jpeg") {
            encoder = match key {
                "progressive" => encoder.progressive(parse_bool(key, value)?),
                "optimize_scans" => encoder.optimize_scans(parse_bool(key, value)?),
                "smoothing" => encoder.smoothing(parse_u8(key, value)?),
                _ => {
                    return Err(Error::InvalidParams(format!("unknown jpeg option `{key}`")));
                }
            };
        }

        if let Some(icc) = img.icc() {
            encoder = encoder.icc_profile(icc.to_vec());
        }

        let img = img.to_u8(Format::Jpeg)?;
        let img = img.without_alpha();
        let samples = img
            .samples()
            .as_u8()
            .ok_or_else(|| Error::Codec("expected 8-bit samples after conversion".into()))?;
        let (w, h) = (img.width(), img.height());
        let out = match img.color() {
            ColorType::Gray => encoder.encode_gray(samples, w, h),
            ColorType::Rgb => encoder.encode_rgb(samples, w, h),
            ColorType::GrayAlpha | ColorType::Rgba => unreachable!("alpha dropped above"),
        };
        out.map_err(|e| Error::Codec(e.to_string()))
    }
}

/// Abstract 0..=100 to mozjpeg's 1..=100. The scales agree; only zero has
/// no meaning on the JPEG side.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn map_quality(q: f32) -> u8 {
    (q.round() as u8).clamp(1, 100)
}

/// Effort to preset. Progressive is the web default (Squoosh, C mozjpeg).
fn map_effort(effort: u8) -> mozjpeg_rs::Preset {
    match effort {
        0..=2 => mozjpeg_rs::Preset::BaselineFastest,
        3..=7 => mozjpeg_rs::Preset::ProgressiveBalanced,
        _ => mozjpeg_rs::Preset::ProgressiveSmallest,
    }
}

fn map_subsampling(s: Subsampling, quality: u8) -> mozjpeg_rs::Subsampling {
    match s {
        Subsampling::S444 => mozjpeg_rs::Subsampling::S444,
        Subsampling::S422 => mozjpeg_rs::Subsampling::S422,
        Subsampling::Auto if quality >= AUTO_444_THRESHOLD => mozjpeg_rs::Subsampling::S444,
        Subsampling::S420 | Subsampling::Auto => mozjpeg_rs::Subsampling::S420,
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool> {
    match value {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(Error::InvalidParams(format!(
            "jpeg:{key} expects a boolean, got `{value}`"
        ))),
    }
}

fn parse_u8(key: &str, value: &str) -> Result<u8> {
    value.parse().map_err(|_| {
        Error::InvalidParams(format!(
            "jpeg:{key} expects an integer 0..=255, got `{value}`"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_mapping_clamps() {
        assert_eq!(map_quality(0.0), 1);
        assert_eq!(map_quality(74.6), 75);
        assert_eq!(map_quality(100.0), 100);
    }

    #[test]
    fn auto_subsampling_follows_quality() {
        assert_eq!(
            map_subsampling(Subsampling::Auto, 75),
            mozjpeg_rs::Subsampling::S420
        );
        assert_eq!(
            map_subsampling(Subsampling::Auto, 90),
            mozjpeg_rs::Subsampling::S444
        );
        assert_eq!(
            map_subsampling(Subsampling::S422, 10),
            mozjpeg_rs::Subsampling::S422
        );
    }

    #[test]
    fn bad_options_are_rejected() {
        assert!(parse_bool("progressive", "maybe").is_err());
        assert!(parse_u8("smoothing", "x").is_err());
        assert!(!parse_bool("progressive", "off").unwrap());
    }
}
