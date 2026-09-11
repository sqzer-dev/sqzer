//! HEIC input via `libheif`, through `libheif-rs` (MIT) over `libheif-sys`
//! (MIT). Unlike the other native backends nothing is vendored: `libheif`
//! and its HEVC decoder `libde265` are LGPL-3.0 and are linked dynamically
//! from the system, found by `pkg-config` on Unix and vcpkg on Windows.
//! Decode only; nobody wants HEIC output (ADR-0001 section 1.1).
//!
//! What `libheif` does for us: it applies the container's own rotation,
//! mirroring and cropping (`irot`, `imir`, `clap`), which is where a phone
//! records orientation, so the Exif orientation tag is deliberately not
//! read as well. Those transformations are applied unconditionally, as
//! the JPEG XL decoder applies its codestream's orientation: libheif can
//! only skip all of them together, and skipping `clap` would hand back the
//! coded frame with its padding. `DecodeOpts::apply_orientation` therefore
//! has no effect here. Grids and tiles are assembled by the library.
//!
//! Known limits of this backend:
//! - An `nclx` colour description (wide-gamut primaries, HDR transfer) is
//!   ignored; samples come back as stored. An ICC profile is kept.
//! - Image sequences yield their primary image.
//! - It decodes only what the system `libheif` has a codec for. A build
//!   without `libde265` recognises HEIC and fails at decode, and AVIF in a
//!   HEIF container stays with the AVIF decoder.

use libheif_rs::color_profile_types::{PROF, R_ICC};
use libheif_rs::{
    ColorProfile, ColorSpace, DecodingOptions, HeifContext, Image as HeifImage, LibHeif, Plane,
    RgbChroma,
};
use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};

/// HEIC decoder. 8-bit sources decode to `u8`, 10 and 12-bit to `u16`
/// scaled to the full 16-bit range. Monochrome files decode to
/// [`ColorType::Gray`], an alpha plane adds an alpha channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeifDecoder;

static DECODER_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Heic,
    name: "libheif-rs",
    animation: false,
    tier: Tier::Native,
};

/// Brands of HEVC-coded HEIF files, still images and sequences. AVIF
/// brands are left to the AVIF decoder on purpose.
const STILL_BRANDS: [&[u8; 4]; 4] = [b"heic", b"heix", b"heim", b"heis"];
const SEQUENCE_BRANDS: [&[u8; 4]; 4] = [b"hevc", b"hevx", b"hevm", b"hevs"];

impl Decoder for HeifDecoder {
    fn caps(&self) -> &DecoderCaps {
        &DECODER_CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        // ISOBMFF: [size:4]["ftyp"][major:4][minor:4][compatible:4]*
        if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
            return None;
        }
        let size = usize::try_from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .ok()?
            .min(bytes.len());
        let major = &bytes[8..12];
        let compatible = bytes.get(16..size).unwrap_or_default().as_chunks::<4>().0;
        let brands = std::iter::once(major).chain(compatible.iter().map(<[u8; 4]>::as_slice));
        let is_still = |b: &[u8]| STILL_BRANDS.iter().any(|s| s.as_slice() == b);
        let is_sequence = |b: &[u8]| SEQUENCE_BRANDS.iter().any(|s| s.as_slice() == b);
        let mut still = false;
        let mut sequence = false;
        for brand in brands {
            still |= is_still(brand);
            sequence |= is_sequence(brand);
        }
        (still || sequence).then_some(FormatInfo {
            format: Format::Heic,
            animated: is_sequence(major),
        })
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        let ctx = HeifContext::read_from_bytes(bytes).ok()?;
        let handle = ctx.primary_image_handle().ok()?;
        Some((handle.width(), handle.height()))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let lib = LibHeif::new();
        let mut ctx = HeifContext::read_from_bytes(bytes).map_err(codec_err)?;
        // Zero decodes on the calling thread; one would spawn a worker.
        ctx.set_max_decoding_threads(0);
        let handle = ctx.primary_image_handle().map_err(codec_err)?;
        opts.check_pixels(handle.width(), handle.height())?;

        let mut options = DecodingOptions::new()
            .ok_or_else(|| Error::Codec("libheif could not allocate decoding options".into()))?;
        options.set_convert_hdr_to_8bit(false);

        let wide = handle.luma_bits_per_pixel() > 8;
        let alpha = handle.has_alpha_channel();
        let mono = matches!(
            handle.preferred_decoding_colorspace(),
            Ok(ColorSpace::Monochrome)
        );
        let icc = handle
            .color_profile_raw()
            .filter(|p| p.profile_type() == R_ICC || p.profile_type() == PROF)
            .map(|p| p.data);

        let (color, samples) = if mono {
            let decoded = lib
                .decode(&handle, ColorSpace::Monochrome, Some(options))
                .map_err(codec_err)?;
            gray_samples(&decoded, alpha, wide, opts)?
        } else {
            let chroma = match (alpha, wide) {
                (false, false) => RgbChroma::Rgb,
                (true, false) => RgbChroma::Rgba,
                (false, true) => RgbChroma::HdrRgbLe,
                (true, true) => RgbChroma::HdrRgbaLe,
            };
            let decoded = lib
                .decode(&handle, ColorSpace::Rgb(chroma), Some(options))
                .map_err(codec_err)?;
            rgb_samples(&decoded, alpha, wide, opts)?
        };
        let (width, height, samples) = samples;
        let mut image = Image::new(width, height, color, samples)?.with_icc(icc);
        if alpha && handle.is_premultiplied_alpha() {
            image = unpremultiply(image);
        }
        Ok(image)
    }
}

/// Width, height and samples of an interleaved RGB or RGBA decode.
fn rgb_samples(
    decoded: &HeifImage,
    alpha: bool,
    wide: bool,
    opts: &DecodeOpts,
) -> Result<(ColorType, (u32, u32, Samples))> {
    let planes = decoded.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| Error::Codec("libheif returned no interleaved plane".into()))?;
    let color = if alpha {
        ColorType::Rgba
    } else {
        ColorType::Rgb
    };
    opts.check_pixels(plane.width, plane.height)?;
    let samples = pack(&plane, color.channels(), wide)?;
    Ok((color, (plane.width, plane.height, samples)))
}

/// Width, height and samples of a monochrome decode, with the alpha plane
/// interleaved in when there is one.
fn gray_samples(
    decoded: &HeifImage,
    alpha: bool,
    wide: bool,
    opts: &DecodeOpts,
) -> Result<(ColorType, (u32, u32, Samples))> {
    let planes = decoded.planes();
    let y = planes
        .y
        .ok_or_else(|| Error::Codec("libheif returned no luma plane".into()))?;
    opts.check_pixels(y.width, y.height)?;
    let luma = pack(&y, 1, wide)?;
    let Some(a) = planes.a.filter(|_| alpha) else {
        return Ok((ColorType::Gray, (y.width, y.height, luma)));
    };
    if (a.width, a.height) != (y.width, y.height) {
        return Err(Error::Codec(format!(
            "alpha plane is {}x{}, luma plane is {}x{}",
            a.width, a.height, y.width, y.height
        )));
    }
    let alpha = pack(&a, 1, wide)?;
    let samples = match (luma, alpha) {
        (Samples::U8(l), Samples::U8(a)) => {
            Samples::U8(l.iter().zip(&a).flat_map(|(&l, &a)| [l, a]).collect())
        }
        (Samples::U16(l), Samples::U16(a)) => {
            Samples::U16(l.iter().zip(&a).flat_map(|(&l, &a)| [l, a]).collect())
        }
        _ => unreachable!("both planes packed at the same depth"),
    };
    Ok((ColorType::GrayAlpha, (y.width, y.height, samples)))
}

/// Copy the used part of each row out of a strided plane. `channels`
/// samples per pixel; `wide` planes hold little-endian 16-bit samples in
/// `bits_per_pixel` bits, scaled here to the full 16-bit range.
fn pack(plane: &Plane<&[u8]>, channels: usize, wide: bool) -> Result<Samples> {
    let width = plane.width as usize;
    let height = plane.height as usize;
    let bytes_per_pixel = channels * if wide { 2 } else { 1 };
    let row_bytes = width * bytes_per_pixel;
    if plane.stride < row_bytes || plane.data.len() < plane.stride * height {
        return Err(Error::Codec(
            "libheif plane is smaller than its geometry".into(),
        ));
    }
    let rows = plane
        .data
        .chunks_exact(plane.stride)
        .take(height)
        .map(|row| &row[..row_bytes]);
    if !wide {
        return Ok(Samples::U8(rows.flatten().copied().collect()));
    }
    let mut out: Vec<u16> = rows
        .flat_map(|row| row.as_chunks::<2>().0)
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    widen(&mut out, plane.bits_per_pixel);
    Ok(Samples::U16(out))
}

/// Scale `bits`-bit samples to 16 bits, replicating the top bits so the
/// endpoints land on 0 and 65535.
fn widen(v: &mut [u16], bits: u8) {
    match bits {
        10 => v.iter_mut().for_each(|s| *s = (*s << 6) | (*s >> 4)),
        12 => v.iter_mut().for_each(|s| *s = (*s << 4) | (*s >> 8)),
        _ => {}
    }
}

/// Undo premultiplied alpha so the colour channels mean the same thing as
/// in every other decoder's output.
fn unpremultiply(image: Image) -> Image {
    let (width, height, color, samples, icc) = image.into_parts();
    let ch = color.channels();
    let samples = match samples {
        Samples::U8(mut v) => {
            for px in v.chunks_exact_mut(ch) {
                let a = u32::from(px[ch - 1]);
                if a > 0 && a < 255 {
                    for c in &mut px[..ch - 1] {
                        *c = u8::try_from((u32::from(*c) * 255 + a / 2) / a).unwrap_or(u8::MAX);
                    }
                }
            }
            Samples::U8(v)
        }
        Samples::U16(mut v) => {
            for px in v.chunks_exact_mut(ch) {
                let a = u64::from(px[ch - 1]);
                if a > 0 && a < 65535 {
                    for c in &mut px[..ch - 1] {
                        *c = u16::try_from((u64::from(*c) * 65535 + a / 2) / a).unwrap_or(u16::MAX);
                    }
                }
            }
            Samples::U16(v)
        }
        float @ Samples::F32(_) => float,
    };
    Image::new(width, height, color, samples)
        .expect("same shape as before")
        .with_icc(icc)
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp(major: [u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let size = u32::try_from(16 + 4 * compatible.len()).unwrap();
        let mut b = size.to_be_bytes().to_vec();
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(&major);
        b.extend_from_slice(&[0, 0, 0, 0]);
        for c in compatible {
            b.extend_from_slice(*c);
        }
        b
    }

    #[test]
    fn probe_reads_hevc_brands_only() {
        let heic = HeifDecoder.probe(&ftyp(*b"heic", &[b"mif1", b"heic"]));
        assert_eq!(
            heic,
            Some(FormatInfo {
                format: Format::Heic,
                animated: false
            })
        );
        // Generic major brand, HEIC only among the compatible ones.
        assert!(HeifDecoder.probe(&ftyp(*b"mif1", &[b"heic"])).is_some());
        // A sequence is animated.
        assert_eq!(
            HeifDecoder
                .probe(&ftyp(*b"hevc", &[b"mif1", b"msf1"]))
                .map(|i| i.animated),
            Some(true)
        );
        // AVIF stays with the AVIF decoder, and other ISOBMFF is not ours.
        assert!(
            HeifDecoder
                .probe(&ftyp(*b"avif", &[b"mif1", b"miaf"]))
                .is_none()
        );
        assert!(HeifDecoder.probe(&ftyp(*b"isom", &[b"mp42"])).is_none());
        assert!(HeifDecoder.probe(b"\x89PNG\r\n\x1a\n").is_none());
    }

    #[test]
    fn widen_hits_the_endpoints() {
        let mut v = [0u16, 1023];
        widen(&mut v, 10);
        assert_eq!(v, [0, 65535]);
        let mut v = [0u16, 4095];
        widen(&mut v, 12);
        assert_eq!(v, [0, 65535]);
    }

    #[test]
    fn pack_drops_the_stride_padding() {
        let plane = Plane {
            data: &[1u8, 2, 3, 0, 0, 4, 5, 6, 0, 0][..],
            width: 1,
            height: 2,
            stride: 5,
            bits_per_pixel: 8,
            storage_bits_per_pixel: 24,
        };
        assert_eq!(
            pack(&plane, 3, false).unwrap(),
            Samples::U8(vec![1, 2, 3, 4, 5, 6])
        );
        let short = Plane {
            data: &[1u8, 2][..],
            ..plane
        };
        assert!(pack(&short, 3, false).is_err());
    }

    #[test]
    fn unpremultiply_recovers_colour() {
        let img =
            Image::from_u8(1, 2, ColorType::Rgba, vec![64, 32, 0, 128, 10, 20, 30, 255]).unwrap();
        let out = unpremultiply(img);
        assert_eq!(
            out.samples().as_u8().unwrap(),
            &[128, 64, 0, 128, 10, 20, 30, 255]
        );
    }
}
