//! JPEG. Decoding via `zune-jpeg` (MIT/Apache/Zlib), encoding via
//! `mozjpeg-rs` (BSD-3): a pure-Rust port of mozjpeg with byte-identical
//! baseline and progressive output and trellis quantisation.

use sqzer_core::codec::{
    CodecOption, Decoder, DecoderCaps, Encoder, EncoderCaps, Format, FormatInfo, Tier,
};
use sqzer_core::image::{ColorType, Image, Orientation};
use sqzer_core::params::{DecodeOpts, EncodeParams, Resolved, Subsampling};
use sqzer_core::{Error, Result};
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::opts::{parse_bool, parse_u8, unknown};

/// JPEG decoder: baseline and progressive, 8-bit. Grayscale files decode to
/// [`ColorType::Gray`]; YCbCr, RGB, CMYK and YCCK all come out as
/// [`ColorType::Rgb`], the backend does the conversion. EXIF orientation is
/// applied, the ICC profile is kept on the image.
#[derive(Debug, Clone, Copy, Default)]
pub struct JpegDecoder;

static DECODER_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Jpeg,
    name: "zune-jpeg",
    animation: false,
    tier: Tier::Portable,
};

const SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];

impl Decoder for JpegDecoder {
    fn caps(&self) -> &DecoderCaps {
        &DECODER_CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        bytes.starts_with(&SOI).then_some(FormatInfo {
            format: Format::Jpeg,
            animated: false,
        })
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        read_headers(bytes).ok().map(|(w, h, _)| (w, h))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        // Headers first, so the pixel limit is checked before any pixel
        // buffer exists and the output layout can follow the input.
        let (width, height, gray) = read_headers(bytes)?;
        opts.check_pixels(width, height)?;
        let options = decoder_options();

        let (color, options) = if gray {
            (
                ColorType::Gray,
                options.jpeg_set_out_colorspace(ColorSpace::Luma),
            )
        } else {
            (ColorType::Rgb, options)
        };
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
        let pixels = decoder.decode().map_err(codec_err)?;
        let icc = decoder.icc_profile();
        let orientation = if opts.apply_orientation {
            decoder
                .exif()
                .and_then(|raw| crate::exif::orientation(raw))
                .unwrap_or_default()
        } else {
            Orientation::default()
        };

        Ok(Image::from_u8(width, height, color, pixels)?
            .with_icc(icc)
            .apply_orientation(orientation))
    }
}

/// The crate's own dimension limits default to 16384 a side; the pixel
/// budget is ours to enforce, so lift them.
fn decoder_options() -> DecoderOptions {
    DecoderOptions::default()
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX)
}

/// Width, height and whether the file is grayscale, from the headers alone.
fn read_headers(bytes: &[u8]) -> Result<(u32, u32, bool)> {
    let mut probe =
        zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), decoder_options());
    probe.decode_headers().map_err(codec_err)?;
    let info = probe
        .info()
        .ok_or_else(|| Error::Codec("jpeg headers missing after decode".into()))?;
    let gray = probe.input_colorspace() == Some(ColorSpace::Luma);
    Ok((u32::from(info.width), u32::from(info.height), gray))
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

/// Above this abstract quality `Subsampling::Auto` stops subsampling chroma.
const AUTO_444_THRESHOLD: u8 = 90;

/// mozjpeg encoder.
#[derive(Debug, Clone, Copy, Default)]
pub struct MozjpegEncoder;

static CAPS: EncoderCaps = EncoderCaps {
    format: Format::Jpeg,
    name: "mozjpeg-rs",
    lossy: true,
    lossless: false,
    alpha: false,
    animation: false,
    bit_depth: &[8],
    hdr: false,
    quality_range: 1.0..=100.0,
    effort_range: 0..=10,
    tier: Tier::Portable,
    options: &[
        CodecOption {
            key: "progressive",
            default: "true",
            help: "progressive scan order; `false` writes baseline",
        },
        CodecOption {
            key: "optimize_scans",
            default: "true",
            help: "search progressive scan scripts for the smallest file",
        },
        CodecOption {
            key: "smoothing",
            default: "0",
            help: "input smoothing `0..=100`, hides dithering noise",
        },
    ],
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
                "progressive" => encoder.progressive(parse_bool("jpeg", key, value)?),
                "optimize_scans" => encoder.optimize_scans(parse_bool("jpeg", key, value)?),
                "smoothing" => encoder.smoothing(parse_u8("jpeg", key, value)?),
                _ => return Err(unknown("jpeg", key)),
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
    fn probe_needs_soi_and_a_marker() {
        assert!(JpegDecoder.probe(&[0xFF, 0xD8, 0xFF, 0xE0]).is_some());
        assert!(JpegDecoder.probe(&[0xFF, 0xD8]).is_none());
        assert!(JpegDecoder.probe(b"\x89PNG").is_none());
    }
}
