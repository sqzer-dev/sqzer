//! WebP via `image-webp` (MIT/Apache). Decoder for lossy, lossless and the
//! first frame of animated files. The lossless encoder lands with ADR-0001
//! item 4.

use std::io::Cursor;

use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Orientation};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};

/// WebP decoder. Output is 8-bit RGB, or RGBA when the file has alpha.
/// EXIF orientation is applied, the ICC profile is kept on the image.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebPDecoder;

static DECODER_CAPS: DecoderCaps = DecoderCaps {
    format: Format::WebP,
    animation: false,
    tier: Tier::Portable,
};

/// Bit in the `VP8X` flags byte that marks an animated file.
const VP8X_ANIMATION: u8 = 0x02;

impl Decoder for WebPDecoder {
    fn caps(&self) -> &DecoderCaps {
        &DECODER_CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        if bytes.len() < 16 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
            return None;
        }
        let animated = &bytes[12..16] == b"VP8X"
            && bytes
                .get(20)
                .is_some_and(|flags| flags & VP8X_ANIMATION != 0);
        Some(FormatInfo {
            format: Format::WebP,
            animated,
        })
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(codec_err)?;
        let (width, height) = decoder.dimensions();
        opts.check_pixels(width, height)?;

        let color = if decoder.has_alpha() {
            ColorType::Rgba
        } else {
            ColorType::Rgb
        };
        let size = decoder
            .output_buffer_size()
            .ok_or_else(|| Error::Codec("output buffer size overflow".into()))?;
        let mut buf = vec![0u8; size];
        decoder.read_image(&mut buf).map_err(codec_err)?;

        let icc = decoder.icc_profile().map_err(codec_err)?;
        let orientation = if opts.apply_orientation {
            decoder
                .exif_metadata()
                .map_err(codec_err)?
                .and_then(|raw| crate::exif::orientation(&raw))
                .unwrap_or_default()
        } else {
            Orientation::default()
        };

        Ok(Image::from_u8(width, height, color, buf)?
            .with_icc(icc)
            .apply_orientation(orientation))
    }
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(chunk: [u8; 4], flags: u8) -> Vec<u8> {
        let mut b = b"RIFF\0\0\0\0WEBP".to_vec();
        b.extend_from_slice(&chunk);
        b.extend_from_slice(&[10, 0, 0, 0, flags, 0, 0, 0]);
        b
    }

    #[test]
    fn probe_reads_the_animation_flag() {
        assert_eq!(
            WebPDecoder.probe(&header(*b"VP8L", 0)),
            Some(FormatInfo {
                format: Format::WebP,
                animated: false
            })
        );
        assert_eq!(
            WebPDecoder.probe(&header(*b"VP8X", 0x10)),
            Some(FormatInfo {
                format: Format::WebP,
                animated: false
            })
        );
        assert_eq!(
            WebPDecoder.probe(&header(*b"VP8X", 0x12)),
            Some(FormatInfo {
                format: Format::WebP,
                animated: true
            })
        );
    }

    #[test]
    fn probe_rejects_other_riff() {
        let mut wave = header(*b"fmt ", 0);
        wave[8..12].copy_from_slice(b"WAVE");
        assert!(WebPDecoder.probe(&wave).is_none());
        assert!(WebPDecoder.probe(b"RIFF").is_none());
    }
}
