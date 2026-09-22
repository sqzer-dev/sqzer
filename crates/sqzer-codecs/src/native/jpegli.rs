//! JPEG encoding via `jpegli`, libjxl's perceptually tuned JPEG encoder,
//! through the `jpegli` crate (BSD-3-Clause) over `jpegli-sys`
//! (BSD-3-Clause), which vendors libjxl 0.10's jpegli and highway and
//! builds them with cmake. Takes over the JPEG format from `mozjpeg-rs`
//! when `native-jpegli` is on; decoding stays with `zune-jpeg`.
//!
//! Error handling is libjpeg's: the library reports an error by unwinding
//! out of the C code, so every call is wrapped in `catch_unwind` and a
//! caught panic becomes [`Error::Codec`]. A build with `panic = "abort"`
//! turns any jpegli error into a process abort; the workspace does not
//! set that.

use std::panic::{AssertUnwindSafe, catch_unwind};

use jpegli::{ColorSpace, Compress, Marker};
use sqzer_core::codec::{Encoder, EncoderCaps, Format, Tier};
use sqzer_core::image::{ColorType, Image};
use sqzer_core::params::{EncodeParams, Resolved, Subsampling};
use sqzer_core::{Error, Result};

use crate::opts::unknown;

/// Above this abstract quality `Subsampling::Auto` stops subsampling chroma.
const AUTO_444_THRESHOLD: u8 = 90;

/// jpegli encoder: progressive, Huffman-optimised, adaptive quantisation.
/// Lossy, 8-bit, no alpha (dropped, not composited), ICC kept.
///
/// Quality maps one to one onto jpegli's `1..=100`, which is calibrated
/// to libjpeg's scale but lands on smaller files at the same score.
/// jpegli has no effort knob, so `effort` is ignored. `Subsampling::Auto`
/// is 4:2:0 below quality 90 and 4:4:4 from there on, the rule the
/// `mozjpeg-rs` backend uses.
///
/// The binding cannot ask jpegli for a baseline scan order or for XYB
/// colour, so there are no options yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct JpegliEncoder;

static ENCODER_CAPS: EncoderCaps = EncoderCaps {
    format: Format::Jpeg,
    name: "jpegli",
    lossy: true,
    lossless: false,
    alpha: false,
    animation: false,
    bit_depth: &[8],
    hdr: false,
    exif: true,
    xmp: true,
    quality_range: 1.0..=100.0,
    effort_range: 0..=0,
    tier: Tier::Native,
    options: &[],
};

impl Encoder for JpegliEncoder {
    fn caps(&self) -> &EncoderCaps {
        &ENCODER_CAPS
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
        if let Some((key, _)) = params.codec_opts("jpeg").next() {
            return Err(unknown("jpeg", key));
        }

        let img = img.to_u8(Format::Jpeg)?;
        let img = img.without_alpha();
        let samples = img
            .samples()
            .as_u8()
            .ok_or_else(|| Error::Codec("expected 8-bit samples after conversion".into()))?;
        let (width, height) = (img.width() as usize, img.height() as usize);
        let (color_space, chroma) = match img.color() {
            ColorType::Gray => (ColorSpace::JCS_GRAYSCALE, None),
            ColorType::Rgb => (
                ColorSpace::JCS_RGB,
                Some(map_subsampling(params.subsampling, quality)),
            ),
            ColorType::GrayAlpha | ColorType::Rgba => unreachable!("alpha dropped above"),
        };
        let icc = img.icc();
        let exif = img
            .exif()
            .map(|e| crate::exif::exif_app1(e, Format::Jpeg))
            .transpose()?;
        let xmp = img
            .xmp()
            .map(|x| crate::exif::xmp_app1(x, Format::Jpeg))
            .transpose()?;

        let encoded = catch_unwind(AssertUnwindSafe(|| -> std::io::Result<Vec<u8>> {
            let mut comp = Compress::new(color_space);
            comp.set_size(width, height);
            comp.set_quality(f32::from(quality));
            comp.set_optimize_coding(true);
            comp.set_progressive_mode();
            if let Some(size) = chroma {
                comp.set_chroma_sampling_pixel_sizes(size, size);
            }
            let mut started = comp.start_compress(Vec::new())?;
            if let Some(icc) = icc {
                started.write_icc_profile(icc);
            }
            for payload in [&exif, &xmp].into_iter().flatten() {
                started.write_marker(Marker::APP(1), payload);
            }
            started.write_scanlines(samples)?;
            started.finish()
        }));
        match encoded {
            Ok(Ok(bytes)) => Ok(bytes),
            Ok(Err(e)) => Err(Error::Codec(e.to_string())),
            Err(panic) => Err(Error::Codec(panic_message(&panic))),
        }
    }
}

/// Abstract `0..=100` to jpegli's `1..=100`. The scales agree; only zero
/// has no meaning on the JPEG side.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn map_quality(q: f32) -> u8 {
    (q.round() as u8).clamp(1, 100)
}

/// Chroma "pixel size" per luma pixel, for both Cb and Cr.
fn map_subsampling(s: Subsampling, quality: u8) -> (u8, u8) {
    match s {
        Subsampling::S444 => (1, 1),
        Subsampling::S422 => (2, 1),
        Subsampling::Auto if quality >= AUTO_444_THRESHOLD => (1, 1),
        Subsampling::S420 | Subsampling::Auto => (2, 2),
    }
}

/// The message jpegli's error manager attached to its unwind.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    let text = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(ToString::to_string))
        .unwrap_or_else(|| "unknown error".into());
    format!("jpegli: {text}")
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
        assert_eq!(map_subsampling(Subsampling::Auto, 75), (2, 2));
        assert_eq!(map_subsampling(Subsampling::Auto, 90), (1, 1));
        assert_eq!(map_subsampling(Subsampling::S422, 10), (2, 1));
    }
}
