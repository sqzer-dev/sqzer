//! AVIF encoding via `libavif` 1.0 with `libaom` as the AV1 encoder,
//! through the `libavif` crate (BSD-2-Clause) over `libavif-sys` and
//! `libaom-sys` (both BSD-2-Clause; libaom itself is BSD-2-Clause plus the
//! Alliance for Open Media patent licence). Both libraries are vendored and built with
//! cmake; x86 builds need `nasm`. Takes over the AVIF format from `ravif`
//! when `native-avif` is on; decoding stays with `re_rav1d`.

use libavif::{AvifImage, RgbPixels, YuvFormat};
use sqzer_core::codec::{CodecOption, Encoder, EncoderCaps, Format, Tier};
use sqzer_core::image::{ColorType, Image};
use sqzer_core::params::{EncodeParams, Resolved, Subsampling};
use sqzer_core::{Error, Result};

use crate::layout::widen_gray;
use crate::opts::unknown;

/// Above this abstract quality `Subsampling::Auto` stops subsampling chroma.
const AUTO_444_THRESHOLD: f32 = 90.0;

/// AVIF encoder over `libavif` and `libaom`. Lossy, 8-bit input and 8-bit
/// payload, 4:4:4, 4:2:2 or 4:2:0, alpha as a separate item, monochrome
/// for gray input.
///
/// Quality maps one to one onto `libavif`'s `0..=100`. Effort `0..=10`
/// maps onto `libaom` speed `10..=0`, the same inversion as the portable
/// `ravif` backend, so the default effort, 6, is speed 4.
/// `Subsampling::Auto` is 4:2:0 below quality 90 and 4:4:4 from there on,
/// the rule the JPEG backend uses.
///
/// What this backend refuses rather than approximates:
/// - `Target::Lossless`. Quality 100 through a YUV matrix is not lossless,
///   and the binding cannot select the identity matrix that would make it
///   so.
/// - An image carrying an ICC profile. The binding writes an sRGB `colr`
///   box and cannot embed a profile, so until the pipeline converts to sRGB
///   and drops it (ADR-0001 D3) such an image is refused, not silently
///   re-tagged.
///
/// Gray plus alpha is encoded as RGBA; the binding's monochrome path has no
/// alpha item.
///
/// Options, all `avif:` prefixed:
/// - `alpha_quality`: `0..=100`, or `auto` (default) to follow the colour
///   quality.
#[derive(Debug, Clone, Copy, Default)]
pub struct LibavifEncoder;

static ENCODER_CAPS: EncoderCaps = EncoderCaps {
    format: Format::Avif,
    name: "libavif",
    lossy: true,
    lossless: false,
    alpha: true,
    animation: false,
    bit_depth: &[8],
    hdr: false,
    quality_range: 0.0..=100.0,
    effort_range: 0..=10,
    tier: Tier::Native,
    options: &[CodecOption {
        key: "alpha_quality",
        default: "auto",
        help: "alpha item quality `0..=100`; `auto` follows the colour quality",
    }],
};

impl Encoder for LibavifEncoder {
    fn caps(&self) -> &EncoderCaps {
        &ENCODER_CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>> {
        let quality = match params.resolved()? {
            Resolved::Quality(q) => map_quality(q)?,
            Resolved::Lossless => return Err(unsupported("lossless output")),
        };
        if img.icc().is_some() {
            return Err(unsupported("an embedded ICC profile"));
        }
        let mut alpha_quality = quality;
        for (key, value) in params.codec_opts("avif") {
            match key {
                "alpha_quality" if value == "auto" => {}
                "alpha_quality" => alpha_quality = crate::opts::parse_percent("avif", key, value)?,
                _ => return Err(unknown("avif", key)),
            }
        }

        let img = img.to_u8(Format::Avif)?;
        let samples = img
            .samples()
            .as_u8()
            .ok_or_else(|| Error::Codec("expected 8-bit samples after conversion".into()))?;
        let (w, h) = (img.width(), img.height());
        // The binding sizes its buffers in `u32`.
        if u64::from(w) * u64::from(h) * 4 > u64::from(u32::MAX) {
            return Err(unsupported("more than a gigasample of input"));
        }
        let yuv = map_subsampling(params.subsampling, quality);
        let image = match img.color() {
            ColorType::Gray => AvifImage::from_luma8(w, h, samples).map_err(codec_err)?,
            ColorType::Rgb | ColorType::Rgba => RgbPixels::new(w, h, samples)
                .map_err(codec_err)?
                .to_image(yuv),
            ColorType::GrayAlpha => RgbPixels::new(w, h, &widen_gray(samples, 2))
                .map_err(codec_err)?
                .to_image(yuv),
        };

        // Single-threaded until the thread budget lands (ADR-0001 D3).
        let mut encoder = libavif::Encoder::new();
        encoder
            .set_max_threads(1)
            .set_quality(quality)
            .set_alpha_quality(alpha_quality)
            .set_speed(map_effort(params.effort));
        encoder
            .encode(&image)
            .map(|data| data.to_vec())
            .map_err(codec_err)
    }
}

/// Abstract `0..=100` to `libavif`'s `0..=100`, rounded to a whole number.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn map_quality(q: f32) -> Result<u8> {
    if q.is_nan() {
        return Err(Error::InvalidParams("quality is NaN".into()));
    }
    Ok(q.round().clamp(0.0, 100.0) as u8)
}

/// Effort `0..=10` to `libaom` speed `10..=0`.
fn map_effort(effort: u8) -> u8 {
    10u8.saturating_sub(effort)
}

fn map_subsampling(s: Subsampling, quality: u8) -> YuvFormat {
    match s {
        Subsampling::S444 => YuvFormat::Yuv444,
        Subsampling::S422 => YuvFormat::Yuv422,
        Subsampling::Auto if f32::from(quality) >= AUTO_444_THRESHOLD => YuvFormat::Yuv444,
        Subsampling::S420 | Subsampling::Auto => YuvFormat::Yuv420,
    }
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported {
        format: Format::Avif,
        what: what.into(),
    }
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_mapping_rounds_and_clamps() {
        assert_eq!(map_quality(-1.0).unwrap(), 0);
        assert_eq!(map_quality(74.6).unwrap(), 75);
        assert_eq!(map_quality(250.0).unwrap(), 100);
        assert!(map_quality(f32::NAN).is_err());
    }

    #[test]
    fn effort_inverts_to_speed() {
        assert_eq!(map_effort(0), 10);
        assert_eq!(map_effort(6), 4);
        assert_eq!(map_effort(10), 0);
        assert_eq!(map_effort(200), 0);
    }

    #[test]
    fn auto_subsampling_follows_quality() {
        assert!(matches!(
            map_subsampling(Subsampling::Auto, 75),
            YuvFormat::Yuv420
        ));
        assert!(matches!(
            map_subsampling(Subsampling::Auto, 90),
            YuvFormat::Yuv444
        ));
        assert!(matches!(
            map_subsampling(Subsampling::S422, 10),
            YuvFormat::Yuv422
        ));
    }
}
