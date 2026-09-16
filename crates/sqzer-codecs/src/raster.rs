//! The simple raster containers, read through the `image` crate (MIT OR
//! Apache-2.0): BMP, TGA, ICO, QOI and PNM. Decoders only; `image`'s
//! encoders are what this project replaces. None of these formats carries
//! an orientation, and only ICO (through its PNG entries) can carry an ICC
//! profile, which is kept.
//!
//! Every probe here is a header check: BMP, ICO, QOI and PNM have magic
//! bytes; TGA has none, so its check is a plausibility test of the header
//! fields and it registers last.

use std::io::Cursor;

use image::ImageDecoder as _;
use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};

/// Decode through any of `image`'s decoders into an [`Image`].
fn decode_with<D: image::ImageDecoder>(
    mut dec: D,
    dec_format: Format,
    opts: &DecodeOpts,
) -> Result<Image> {
    let (width, height) = dec.dimensions();
    opts.check_pixels(width, height)?;
    let (color, wide) = match dec.color_type() {
        image::ColorType::L8 => (ColorType::Gray, false),
        image::ColorType::La8 => (ColorType::GrayAlpha, false),
        image::ColorType::Rgb8 => (ColorType::Rgb, false),
        image::ColorType::Rgba8 => (ColorType::Rgba, false),
        image::ColorType::L16 => (ColorType::Gray, true),
        image::ColorType::La16 => (ColorType::GrayAlpha, true),
        image::ColorType::Rgb16 => (ColorType::Rgb, true),
        image::ColorType::Rgba16 => (ColorType::Rgba, true),
        other => {
            return Err(Error::Codec(format!(
                "{dec_format}: {other:?} samples are not supported"
            )));
        }
    };
    let icc = dec.icc_profile().ok().flatten();
    let len = usize::try_from(dec.total_bytes())
        .map_err(|_| Error::Codec("image too large for this platform".into()))?;
    let mut buf = vec![0u8; len];
    dec.read_image(&mut buf).map_err(codec_err)?;
    let samples = if wide {
        // `image` writes 16-bit samples in native byte order.
        Samples::U16(
            buf.as_chunks::<2>()
                .0
                .iter()
                .map(|&b| u16::from_ne_bytes(b))
                .collect(),
        )
    } else {
        Samples::U8(buf)
    };
    Ok(Image::new(width, height, color, samples)?.with_icc(icc))
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

macro_rules! raster_decoder {
    ($(#[$doc:meta])* $name:ident, $format:expr, $crate_name:literal, $probe:ident, $decoder:path) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $name;

        impl Decoder for $name {
            fn caps(&self) -> &DecoderCaps {
                static CAPS: DecoderCaps = DecoderCaps {
                    format: $format,
                    name: $crate_name,
                    animation: false,
                    tier: Tier::Portable,
                };
                &CAPS
            }

            fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
                $probe(bytes).then_some(FormatInfo {
                    format: $format,
                    animated: false,
                })
            }

            fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
                self.probe(bytes)?;
                $decoder(Cursor::new(bytes)).ok().map(|d| d.dimensions())
            }

            fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
                let dec = $decoder(Cursor::new(bytes)).map_err(codec_err)?;
                decode_with(dec, $format, opts)
            }
        }
    };
}

raster_decoder!(
    /// BMP decoder. 1 to 32 bits per pixel, RLE included; alpha from
    /// 32-bit files with an alpha mask.
    BmpDecoder,
    Format::Bmp,
    "image",
    is_bmp,
    image::codecs::bmp::BmpDecoder::new
);
raster_decoder!(
    /// TGA decoder. Uncompressed and RLE, true colour, gray and
    /// colour-mapped.
    TgaDecoder,
    Format::Tga,
    "image",
    is_tga,
    image::codecs::tga::TgaDecoder::new
);
raster_decoder!(
    /// ICO and CUR decoder. The largest entry is decoded, BMP or PNG
    /// encoded.
    IcoDecoder,
    Format::Ico,
    "image",
    is_ico,
    image::codecs::ico::IcoDecoder::new
);
raster_decoder!(
    /// QOI decoder, RGB and RGBA.
    QoiDecoder,
    Format::Qoi,
    "image",
    is_qoi,
    image::codecs::qoi::QoiDecoder::new
);
raster_decoder!(
    /// PNM decoder: PBM, PGM, PPM, ASCII or binary, and PAM; 8 and 16-bit.
    PnmDecoder,
    Format::Pnm,
    "image",
    is_pnm,
    image::codecs::pnm::PnmDecoder::new
);

fn is_bmp(bytes: &[u8]) -> bool {
    // File header (14 bytes) plus the smallest info header (12).
    bytes.len() >= 26 && bytes.starts_with(b"BM")
}

fn is_ico(bytes: &[u8]) -> bool {
    // Reserved zero, type 1 (icon) or 2 (cursor), at least one entry.
    bytes.len() >= 22
        && bytes[0] == 0
        && bytes[1] == 0
        && matches!(bytes[2], 1 | 2)
        && bytes[3] == 0
        && u16::from_le_bytes([bytes[4], bytes[5]]) > 0
}

fn is_qoi(bytes: &[u8]) -> bool {
    bytes.len() >= 14 && bytes.starts_with(b"qoif")
}

fn is_pnm(bytes: &[u8]) -> bool {
    // `P1` to `P7` followed by whitespace or a comment.
    bytes.len() >= 3
        && bytes[0] == b'P'
        && (b'1'..=b'7').contains(&bytes[1])
        && (bytes[2].is_ascii_whitespace() || bytes[2] == b'#')
}

/// TGA has no magic number. A TGA 2.0 file ends with a signature; older
/// files are recognised by a header whose fields agree with each other:
/// the image type, whether it says a colour map follows, and the bits per
/// pixel that type allows. Uncompressed files also have to be at least as
/// long as their pixel data.
fn is_tga(bytes: &[u8]) -> bool {
    if bytes.len() < 18 {
        return false;
    }
    if bytes.ends_with(b"TRUEVISION-XFILE.\0") {
        return true;
    }
    let id_len = usize::from(bytes[0]);
    let cmap_type = bytes[1];
    let image_type = bytes[2];
    let cmap_len = usize::from(u16::from_le_bytes([bytes[5], bytes[6]]));
    let cmap_depth = usize::from(bytes[7]);
    let width = usize::from(u16::from_le_bytes([bytes[12], bytes[13]]));
    let height = usize::from(u16::from_le_bytes([bytes[14], bytes[15]]));
    let bpp = bytes[16];
    let descriptor = bytes[17];
    let (mapped, rle) = match image_type {
        1 => (true, false),
        9 => (true, true),
        2 | 3 => (false, false),
        10 | 11 => (false, true),
        _ => return false,
    };
    let bpp_ok = match image_type {
        1 | 9 => matches!(bpp, 8 | 16),
        3 | 11 => matches!(bpp, 8 | 16),
        _ => matches!(bpp, 15 | 16 | 24 | 32),
    };
    if !bpp_ok
        || width == 0
        || height == 0
        || descriptor & 0xC0 != 0
        || (descriptor & 0x0F) > 8
        || cmap_type > 1
        || mapped != (cmap_type == 1)
        || (cmap_type == 1 && (cmap_len == 0 || !matches!(cmap_depth, 15 | 16 | 24 | 32)))
        || (cmap_type == 0 && cmap_len != 0)
    {
        return false;
    }
    if rle {
        return true;
    }
    let cmap_bytes = if cmap_type == 1 {
        cmap_len * cmap_depth.div_ceil(8)
    } else {
        0
    };
    let pixel_bytes = width * height * usize::from(bpp).div_ceil(8);
    bytes.len() >= 18 + id_len + cmap_bytes + pixel_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tga_plausibility_rejects_the_other_formats() {
        // Every other container this crate knows starts with bytes a TGA
        // header cannot have.
        for other in [
            &b"\xFF\xD8\xFF\xE0\x00\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0"[..],
            b"\x89PNG\r\n\x1A\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08",
            b"GIF89a\x01\0\x01\0\0\0\0\x2C\0\0\0\0\x01\0\x01\0\0\x02\0\x3B",
            b"RIFF\x10\0\0\0WEBPVP8L\x04\0\0\0\x2F\0\0\0",
            b"\0\0\0\x1Cftypavif\0\0\0\0avifmif1miaf",
            b"BM\x3A\0\0\0\0\0\0\0\x36\0\0\0\x28\0\0\0\x01\0\0\0\x01\0",
            b"\0\0\x01\0\x01\0\x30\x20\0\0\x01\0\x20\0\x68\x06\0\0\x16\0\0\0",
            b"qoif\0\0\0\x01\0\0\0\x01\x03\0",
            b"P6\n1 1\n255\n\0\0\0\0\0\0\0\0\0\0",
            b"II\x2A\0\x08\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        ] {
            assert!(!is_tga(other), "{other:?}");
        }
    }

    #[test]
    fn tga_plausibility_accepts_a_real_header() {
        // 1 x 1 uncompressed true colour, 24 bpp, with its three bytes.
        let mut b = vec![0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 24, 0];
        assert!(!is_tga(&b), "pixel data missing");
        b.extend_from_slice(&[1, 2, 3]);
        assert!(is_tga(&b));
        // RLE cannot be length-checked, so the header alone decides.
        b[2] = 10;
        b.truncate(18);
        assert!(is_tga(&b));
        // A footer settles it whatever the header says.
        let mut f = vec![0u8; 26];
        f.extend_from_slice(b"TRUEVISION-XFILE.\0");
        assert!(is_tga(&f));
    }

    #[test]
    fn magic_probes() {
        assert!(is_pnm(b"P4 1 1\n\x80"));
        assert!(is_pnm(b"P7\nWIDTH 1\n"));
        assert!(!is_pnm(b"P8 1 1\n"));
        assert!(!is_pnm(b"PNG"));
        assert!(is_ico(&[
            0, 0, 2, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 22, 0, 0, 0
        ]));
        assert!(!is_ico(&[
            0, 0, 3, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 22, 0, 0, 0
        ]));
        assert!(!is_qoi(b"qoif"));
        assert!(!is_bmp(b"BM"));
    }
}
