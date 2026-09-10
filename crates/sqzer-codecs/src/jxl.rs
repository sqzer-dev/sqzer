//! JPEG XL via `jxl-oxide` (MIT/Apache). Decoder only: no permissive
//! pure-Rust JXL encoder exists (ADR-0001 section 1.1).
//!
//! Colour handling: an image with its own ICC profile decodes in that
//! profile's space and keeps the profile on the [`Image`]. Everything else,
//! including XYB-encoded (lossy) files and enumerated wide-gamut or HDR
//! encodings, is rendered to sRGB by the backend and carries no profile.
//! The codestream's own orientation field is always applied by the backend,
//! independent of `DecodeOpts::apply_orientation`.

use jxl_oxide::color::{EnumColourEncoding, RenderingIntent};
use jxl_oxide::image::BitDepth;
use jxl_oxide::{InitializeResult, JxlImage, JxlThreadPool, PixelFormat, UninitializedJxlImage};
use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};

/// JPEG XL decoder. 8-bit sources decode to `u8`, anything deeper (including
/// float samples) to `u16`. Animated files yield the first keyframe.
#[derive(Debug, Clone, Copy, Default)]
pub struct JxlDecoder;

static DECODER_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Jxl,
    name: "jxl-oxide",
    animation: false,
    tier: Tier::Portable,
};

/// Bare codestream signature.
const CODESTREAM: [u8; 2] = [0xFF, 0x0A];
/// ISOBMFF container signature.
const CONTAINER: [u8; 12] = [
    0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
];
/// How much to feed the header parser per step, so probing a large file
/// does not copy all of it.
const CHUNK: usize = 4096;

impl Decoder for JxlDecoder {
    fn caps(&self) -> &DecoderCaps {
        &DECODER_CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        if !(bytes.starts_with(&CODESTREAM) || bytes.starts_with(&CONTAINER)) {
            return None;
        }
        // The signature is enough to claim the format; the header decides
        // animation and is cheap to parse on its own.
        let animated = parse_header(bytes)
            .is_ok_and(|(image, _)| image.image_header().metadata.animation.is_some());
        Some(FormatInfo {
            format: Format::Jxl,
            animated,
        })
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        parse_header(bytes)
            .ok()
            .map(|(image, _)| (image.width(), image.height()))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let (mut image, mut pos) = parse_header(bytes)?;
        opts.check_pixels(image.width(), image.height())?;

        let format = image.pixel_format();
        if format.has_black() {
            return Err(Error::Codec("CMYK JPEG XL is not supported".into()));
        }
        let color = match format {
            PixelFormat::Gray => ColorType::Gray,
            PixelFormat::Graya => ColorType::GrayAlpha,
            PixelFormat::Rgb => ColorType::Rgb,
            PixelFormat::Rgba => ColorType::Rgba,
            PixelFormat::Cmyk | PixelFormat::Cmyka => unreachable!("rejected above"),
        };

        let metadata = &image.image_header().metadata;
        let keep_icc = image.original_icc().is_some() && !metadata.xyb_encoded;
        let wide = !matches!(
            metadata.bit_depth,
            BitDepth::IntegerSample { bits_per_sample } if bits_per_sample <= 8
        );
        if !keep_icc {
            let intent = RenderingIntent::Relative;
            image.request_color_encoding(if format.is_grayscale() {
                EnumColourEncoding::gray_srgb(intent)
            } else {
                EnumColourEncoding::srgb(intent)
            });
        }

        // The rest of the file: frames.
        while pos < bytes.len() && !image.is_loading_done() {
            let consumed = image.feed_bytes(&bytes[pos..]).map_err(codec_err)?;
            if consumed == 0 {
                break;
            }
            pos += consumed;
        }
        image.finalize().map_err(codec_err)?;

        let render = image.render_frame(0).map_err(codec_err)?;
        let mut stream = render.stream();
        let (width, height) = (stream.width(), stream.height());
        let channels = usize::try_from(stream.channels()).expect("channel count fits");
        if channels != color.channels() {
            return Err(Error::Codec(format!(
                "rendered {channels} channels for {color:?}"
            )));
        }
        let len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(channels))
            .ok_or_else(|| Error::Codec("output buffer size overflow".into()))?;

        let samples = if wide {
            let mut buf = vec![0u16; len];
            stream.write_to_buffer(&mut buf);
            Samples::U16(buf)
        } else {
            let mut buf = vec![0u8; len];
            stream.write_to_buffer(&mut buf);
            Samples::U8(buf)
        };
        let icc = if keep_icc {
            image.original_icc().map(<[u8]>::to_vec)
        } else {
            None
        };
        Ok(Image::new(width, height, color, samples)?.with_icc(icc))
    }
}

/// Parse the image header (and embedded ICC) without touching frame data.
/// Returns the initialised decoder and how many input bytes it consumed.
fn parse_header(bytes: &[u8]) -> Result<(JxlImage, usize)> {
    let mut uninit: UninitializedJxlImage = JxlImage::builder()
        .pool(JxlThreadPool::none())
        .build_uninit();
    let mut pos = 0;
    let mut end = CHUNK.min(bytes.len());
    loop {
        let consumed = uninit.feed_bytes(&bytes[pos..end]).map_err(codec_err)?;
        pos += consumed;
        match uninit.try_init().map_err(codec_err)? {
            InitializeResult::Initialized(image) => return Ok((image, pos)),
            InitializeResult::NeedMoreData(more) => uninit = more,
        }
        if end == bytes.len() && consumed == 0 {
            return Err(Error::Codec("truncated before the image header".into()));
        }
        end = end.saturating_add(CHUNK).min(bytes.len());
    }
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_accepts_both_signatures_only() {
        assert!(JxlDecoder.probe(&CODESTREAM).is_some());
        assert!(JxlDecoder.probe(&CONTAINER).is_some());
        assert!(JxlDecoder.probe(&[0xFF, 0xD8, 0xFF]).is_none());
        assert!(JxlDecoder.probe(&CONTAINER[..11]).is_none());
    }

    #[test]
    fn truncated_header_is_a_codec_error() {
        assert!(matches!(
            JxlDecoder.decode(&CODESTREAM, &DecodeOpts::default()),
            Err(Error::Codec(_))
        ));
    }
}
