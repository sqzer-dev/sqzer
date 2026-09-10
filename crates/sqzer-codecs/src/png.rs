//! PNG via the `png` crate (MIT/Apache). Decoder and a baseline encoder.
//!
//! The encoder here is the plain `png` writer with adaptive filtering. It is
//! correct and fast but not small. On desktop targets the registry uses
//! [`crate::oxipng`] instead; this one is registered on wasm32, where
//! `oxipng`'s C dependency cannot go (ADR-0002), and stays public for
//! callers who want the fast path.

use std::borrow::Cow;

use sqzer_core::codec::{Decoder, DecoderCaps, Encoder, EncoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::{DecodeOpts, EncodeParams};
use sqzer_core::{Error, Result};

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// PNG and APNG decoder. APNG yields the first frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct PngDecoder;

static DECODER_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Png,
    name: "png",
    animation: false,
    tier: Tier::Portable,
};

impl Decoder for PngDecoder {
    fn caps(&self) -> &DecoderCaps {
        &DECODER_CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        bytes.starts_with(&SIGNATURE).then(|| FormatInfo {
            format: Format::Png,
            animated: has_actl_chunk(bytes),
        })
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        // IHDR is always the first chunk: 8 signature bytes, 4 length, 4
        // type, then width and height as big-endian u32.
        let ihdr = bytes.get(12..24)?;
        if &ihdr[..4] != b"IHDR" {
            return None;
        }
        let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        Some((be(&ihdr[4..8]), be(&ihdr[8..12])))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        // Palette to RGB, sub-byte gray to 8-bit, tRNS to alpha. 16-bit stays.
        decoder.set_transformations(png::Transformations::EXPAND);
        // The crate's default allocation budget is 64 MiB, far below a legal
        // image at the pixel limit. Budget for RGBA16 at `max_pixels`.
        decoder.set_limits(png::Limits {
            bytes: usize::try_from(opts.max_pixels.saturating_mul(8)).unwrap_or(usize::MAX),
        });
        let mut reader = decoder.read_info().map_err(codec_err)?;

        let (width, height, icc) = {
            let info = reader.info();
            opts.check_pixels(info.width, info.height)?;
            let icc = info.icc_profile.as_ref().map(|c| c.to_vec());
            (info.width, info.height, icc)
        };

        let (color_type, bit_depth) = reader.output_color_type();
        let color = match color_type {
            png::ColorType::Grayscale => ColorType::Gray,
            png::ColorType::GrayscaleAlpha => ColorType::GrayAlpha,
            png::ColorType::Rgb => ColorType::Rgb,
            png::ColorType::Rgba => ColorType::Rgba,
            png::ColorType::Indexed => {
                return Err(Error::Codec("palette survived EXPAND".into()));
            }
        };

        let size = reader
            .output_buffer_size()
            .ok_or_else(|| Error::Codec("output buffer size overflow".into()))?;
        let mut buf = vec![0u8; size];
        let frame = reader.next_frame(&mut buf).map_err(codec_err)?;
        buf.truncate(frame.buffer_size());

        let samples = match bit_depth {
            png::BitDepth::Eight => Samples::U8(buf),
            png::BitDepth::Sixteen => Samples::U16(
                buf.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&b| u16::from_be_bytes(b))
                    .collect(),
            ),
            other => {
                return Err(Error::Codec(format!("bit depth {other:?} survived EXPAND")));
            }
        };

        Ok(Image::new(width, height, color, samples)?.with_icc(icc))
    }
}

/// Walk chunk headers up to the first IDAT looking for an acTL chunk.
fn has_actl_chunk(bytes: &[u8]) -> bool {
    let mut pos = SIGNATURE.len();
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        let kind = &bytes[pos + 4..pos + 8];
        match kind {
            b"acTL" => return true,
            b"IDAT" | b"IEND" => return false,
            _ => {}
        }
        // length + type + data + crc
        pos = match (len as usize)
            .checked_add(12)
            .and_then(|n| pos.checked_add(n))
        {
            Some(next) => next,
            None => return false,
        };
    }
    false
}

/// PNG encoder: lossless, 8 or 16 bit, any channel layout. Fast, not
/// small; see the module docs.
#[derive(Debug, Clone, Copy, Default)]
pub struct PngEncoder;

static ENCODER_CAPS: EncoderCaps = EncoderCaps {
    format: Format::Png,
    name: "png",
    lossy: false,
    lossless: true,
    alpha: true,
    animation: false,
    bit_depth: &[8, 16],
    hdr: false,
    quality_range: 100.0..=100.0,
    effort_range: 0..=10,
    tier: Tier::Portable,
    options: &[],
};

impl Encoder for PngEncoder {
    fn caps(&self) -> &EncoderCaps {
        &ENCODER_CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>> {
        // PNG is lossless whatever the target says; a quality only has to be
        // resolved, it is not used.
        params.resolved()?;
        if let Some((key, _)) = params.codec_opts("png").next() {
            return Err(Error::InvalidParams(format!("unknown png option `{key}`")));
        }

        let color = match img.color() {
            ColorType::Gray => png::ColorType::Grayscale,
            ColorType::GrayAlpha => png::ColorType::GrayscaleAlpha,
            ColorType::Rgb => png::ColorType::Rgb,
            ColorType::Rgba => png::ColorType::Rgba,
        };
        let (bit_depth, bytes): (png::BitDepth, Cow<'_, [u8]>) = match img.samples() {
            Samples::U8(v) => (png::BitDepth::Eight, Cow::Borrowed(v)),
            Samples::U16(v) => (
                png::BitDepth::Sixteen,
                Cow::Owned(v.iter().flat_map(|s| s.to_be_bytes()).collect()),
            ),
            Samples::F32(_) => {
                return Err(Error::Unsupported {
                    format: Format::Png,
                    what: "float (HDR) samples".into(),
                });
            }
        };

        let mut info = png::Info::with_size(img.width(), img.height());
        info.color_type = color;
        info.bit_depth = bit_depth;
        info.icc_profile = img.icc().map(Cow::Borrowed);

        let mut out = Vec::new();
        let mut encoder = png::Encoder::with_info(&mut out, info).map_err(codec_err)?;
        encoder.set_compression(match params.effort {
            0 => png::Compression::Fastest,
            1..=3 => png::Compression::Fast,
            4..=7 => png::Compression::Balanced,
            _ => png::Compression::High,
        });
        encoder.set_filter(png::Filter::Adaptive);
        let mut writer = encoder.write_header().map_err(codec_err)?;
        writer.write_image_data(&bytes).map_err(codec_err)?;
        writer.finish().map_err(codec_err)?;
        Ok(out)
    }
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_rejects_other_bytes() {
        assert!(PngDecoder.probe(b"\xFF\xD8\xFF").is_none());
        assert!(PngDecoder.probe(&SIGNATURE[..4]).is_none());
    }

    #[test]
    fn actl_detection_stops_at_idat() {
        let mut bytes = SIGNATURE.to_vec();
        // IHDR with 13 bytes payload, then IDAT, then acTL after (never read).
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&[0; 13 + 4]);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"IDAT");
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(b"acTL");
        assert!(!has_actl_chunk(&bytes));

        let mut animated = SIGNATURE.to_vec();
        animated.extend_from_slice(&8u32.to_be_bytes());
        animated.extend_from_slice(b"acTL");
        assert!(has_actl_chunk(&animated));
        assert_eq!(
            PngDecoder.probe(&animated),
            Some(FormatInfo {
                format: Format::Png,
                animated: true
            })
        );
    }

    #[test]
    fn actl_detection_survives_truncation() {
        let mut bytes = SIGNATURE.to_vec();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        bytes.extend_from_slice(b"tEXt");
        assert!(!has_actl_chunk(&bytes));
    }
}
