//! PNG decoding via the `png` crate (MIT/Apache). The encoder is
//! [`crate::oxipng`], on every target.
//!
//! The decoder keeps ICC, EXIF (`eXIf`) and XMP (`iTXt`), wherever in the
//! file the chunks sit, and applies the EXIF orientation when
//! `DecodeOpts::apply_orientation` asks for it, as the JPEG decoder does.

use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Metadata, Orientation, Samples};
use sqzer_core::params::DecodeOpts;
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

        let (width, height) = {
            let info = reader.info();
            opts.check_pixels(info.width, info.height)?;
            (info.width, info.height)
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
        // `eXIf` and XMP may follow the image data; reading to `IEND`
        // collects them. The pixels are already in hand, so damage past
        // them costs the trailing metadata, not the decode.
        let _ = reader.finish();
        let meta = {
            let info = reader.info();
            Metadata {
                icc: info.icc_profile.as_ref().map(|c| c.to_vec()),
                exif: info.exif_metadata.as_ref().map(|c| c.to_vec()),
                xmp: info
                    .utf8_text
                    .iter()
                    .find(|t| t.keyword == XMP_KEYWORD)
                    .and_then(|t| t.get_text().ok())
                    .map(String::into_bytes),
            }
        };
        let orientation = if opts.apply_orientation {
            meta.exif
                .as_deref()
                .and_then(crate::exif::orientation)
                .unwrap_or(Orientation::Normal)
        } else {
            Orientation::Normal
        };

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

        Ok(Image::new(width, height, color, samples)?
            .with_metadata(meta)
            .apply_orientation(orientation))
    }
}

/// The `iTXt` keyword that carries XMP in a PNG.
pub(crate) const XMP_KEYWORD: &str = "XML:com.adobe.xmp";

/// An XMP packet as UTF-8 for an `iTXt` chunk.
///
/// # Errors
/// [`Error::Unsupported`] for a packet that is not UTF-8.
pub(crate) fn xmp_text(xmp: &[u8]) -> Result<String> {
    String::from_utf8(xmp.to_vec()).map_err(|_| Error::Unsupported {
        format: Format::Png,
        what: "an XMP packet that is not UTF-8".into(),
    })
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

    /// A 2 x 1 RGB PNG whose EXIF says "rotate 90 clockwise".
    fn rotated_png() -> Vec<u8> {
        let img = Image::from_u8(2, 1, ColorType::Rgb, vec![10, 20, 30, 40, 50, 60])
            .unwrap()
            .with_exif(Some(crate::exif::tiff_with_orientation(6)));
        let params = sqzer_core::params::EncodeParams {
            target: sqzer_core::params::Target::Lossless,
            ..Default::default()
        };
        sqzer_core::codec::Encoder::encode(&crate::oxipng::OxipngEncoder, &img, &params).unwrap()
    }

    /// The byte range of the first chunk of type `kind`, length, type, data
    /// and CRC included.
    fn chunk(png: &[u8], kind: [u8; 4]) -> std::ops::Range<usize> {
        let mut at = SIGNATURE.len();
        loop {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            if png[at + 4..at + 8] == kind {
                return at..at + 12 + len;
            }
            at += 12 + len;
        }
    }

    #[test]
    fn exif_orientation_is_applied_and_reset() {
        let img = PngDecoder
            .decode(&rotated_png(), &DecodeOpts::default())
            .unwrap();
        assert_eq!((img.width(), img.height()), (1, 2));
        assert_eq!(img.samples().as_u8(), Some(&[10, 20, 30, 40, 50, 60][..]));
        assert_eq!(
            crate::exif::orientation(img.exif().unwrap()),
            Some(Orientation::Normal)
        );
    }

    #[test]
    fn no_auto_orient_keeps_pixels_and_tag() {
        let opts = DecodeOpts {
            apply_orientation: false,
            ..DecodeOpts::default()
        };
        let img = PngDecoder.decode(&rotated_png(), &opts).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
        assert_eq!(
            crate::exif::orientation(img.exif().unwrap()),
            Some(Orientation::Rotate90)
        );
    }

    #[test]
    fn exif_after_the_image_data_is_read() {
        // Move `eXIf` from before `IDAT` to just before `IEND`, where some
        // writers put it.
        let png = rotated_png();
        let exif = chunk(&png, *b"eXIf");
        let mut moved: Vec<u8> = [&png[..exif.start], &png[exif.end..]].concat();
        let iend = chunk(&moved, *b"IEND").start;
        moved.splice(iend..iend, png[exif].iter().copied());
        assert!(chunk(&moved, *b"eXIf").start > chunk(&moved, *b"IDAT").start);

        let img = PngDecoder.decode(&moved, &DecodeOpts::default()).unwrap();
        assert_eq!((img.width(), img.height()), (1, 2));
        assert!(img.exif().is_some());
    }
}
