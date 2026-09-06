//! AVIF decoding: `avif-parse` (MPL-2.0) splits the container, `re_rav1d`
//! (BSD-2) decodes the AV1 payloads, `yuv` (BSD-3/Apache) converts to RGB.
//! The `ravif` encoder lands with ADR-0001 item 4.
//!
//! Desktop only. Upstream `rav1d` does not compile for wasm32, so this
//! module is `cfg`'d out there and the `avif` feature adds no decoder to the
//! WASM build (ADR-0001 D6 names this as the accepted gap).
//!
//! Known limits of this backend:
//! - `avif-parse` does not surface the `colr` box, `irot`/`imir` or the
//!   Exif item, so ICC profiles, wide-gamut primaries and orientation are
//!   ignored. Samples are returned as sRGB with the matrix coefficients and
//!   range the AV1 sequence header declares.
//! - Animated files (`avis`) yield their first frame.
//! - Decoding is single-threaded on the pure-Rust paths; assembly is
//!   compiled out so no `nasm` is needed.

use std::io::Cursor;

use re_rav1d::dav1d::{
    Decoder as Av1Decoder, Error as Av1Error, Picture, PixelLayout, PlanarImageComponent, Settings,
    pixel,
};
use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};
use yuv::{YuvGrayImage, YuvPlanarImage, YuvRange, YuvStandardMatrix};

/// AVIF decoder. 8-bit sources decode to `u8`, 10 and 12-bit to `u16`
/// scaled to the full 16-bit range. Monochrome files decode to
/// [`ColorType::Gray`], an alpha item adds an alpha channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct AvifDecoder;

static DECODER_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Avif,
    animation: false,
    tier: Tier::Portable,
};

impl Decoder for AvifDecoder {
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
        let is_avif = |b: &[u8]| b == b"avif" || b == b"avis";
        (is_avif(major) || compatible.iter().any(|b| is_avif(b))).then_some(FormatInfo {
            format: Format::Avif,
            animated: major == b"avis",
        })
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let data = avif_parse::read_avif(&mut Cursor::new(bytes)).map_err(codec_err)?;
        let meta = data.primary_item_metadata().map_err(codec_err)?;
        opts.check_pixels(meta.max_frame_width.get(), meta.max_frame_height.get())?;

        let color = decode_av1(&data.primary_item)?;
        let (width, height) = (color.width(), color.height());
        opts.check_pixels(width, height)?;
        let (color_type, samples) = to_rgb(&color)?;

        let Some(alpha_obu) = &data.alpha_item else {
            return Image::new(width, height, color_type, samples);
        };
        let alpha = decode_av1(alpha_obu)?;
        if (alpha.width(), alpha.height()) != (width, height) {
            return Err(Error::Codec(format!(
                "alpha item is {}x{}, colour item is {width}x{height}",
                alpha.width(),
                alpha.height()
            )));
        }
        let (_, alpha) = to_rgb(&alpha)?;
        let with_alpha = match color_type {
            ColorType::Gray => ColorType::GrayAlpha,
            _ => ColorType::Rgba,
        };
        let ch = color_type.channels();
        let premultiplied = data.premultiplied_alpha;
        let samples = match (samples, alpha) {
            (Samples::U8(c), Samples::U8(a)) => Samples::U8(interleave(&c, &a, ch, premultiplied)),
            (Samples::U8(c), Samples::U16(a)) => {
                let a: Vec<u8> = a.iter().map(|&v| (v >> 8) as u8).collect();
                Samples::U8(interleave(&c, &a, ch, premultiplied))
            }
            (Samples::U16(c), Samples::U16(a)) => {
                Samples::U16(interleave(&c, &a, ch, premultiplied))
            }
            (Samples::U16(c), Samples::U8(a)) => {
                let a: Vec<u16> = a.iter().map(|&v| u16::from(v) * 257).collect();
                Samples::U16(interleave(&c, &a, ch, premultiplied))
            }
            (Samples::F32(_), _) | (_, Samples::F32(_)) => unreachable!("AV1 has no float"),
        };
        Image::new(width, height, with_alpha, samples)
    }
}

/// Decode one still AV1 temporal unit to a picture.
fn decode_av1(obu: &[u8]) -> Result<Picture> {
    let mut settings = Settings::new();
    settings.set_n_threads(1);
    settings.set_max_frame_delay(1);
    let mut decoder = Av1Decoder::with_settings(&settings).map_err(codec_err)?;
    match decoder.send_data(obu.to_vec(), None, None, None) {
        Ok(()) | Err(Av1Error::Again) => {}
        Err(e) => return Err(codec_err(e)),
    }
    loop {
        match decoder.get_picture() {
            Ok(picture) => return Ok(picture),
            Err(Av1Error::Again) => match decoder.send_pending_data() {
                Ok(()) => {
                    return decoder.get_picture().map_err(|e| match e {
                        Av1Error::Again => Error::Codec("AV1 payload holds no picture".into()),
                        other => codec_err(other),
                    });
                }
                Err(Av1Error::Again) => {}
                Err(e) => return Err(codec_err(e)),
            },
            Err(e) => return Err(codec_err(e)),
        }
    }
}

/// Interleave one alpha sample after every pixel, undoing premultiplication
/// on the way when the container says so.
fn interleave<T>(color: &[T], alpha: &[T], ch: usize, premultiplied: bool) -> Vec<T>
where
    T: Copy + Into<u32> + TryFrom<u32> + Bounded,
{
    let max: u32 = T::MAX.into();
    let mut out = Vec::with_capacity(color.len() + alpha.len());
    for (px, &a) in color.chunks_exact(ch).zip(alpha) {
        let a32: u32 = a.into();
        if premultiplied && a32 > 0 && a32 < max {
            out.extend(px.iter().map(|&c| {
                let c32: u32 = c.into();
                T::try_from(((c32 * max + a32 / 2) / a32).min(max)).unwrap_or(T::MAX)
            }));
        } else {
            out.extend_from_slice(px);
        }
        out.push(a);
    }
    out
}

/// The largest value of a sample type.
trait Bounded {
    const MAX: Self;
}
impl Bounded for u8 {
    const MAX: Self = u8::MAX;
}
impl Bounded for u16 {
    const MAX: Self = u16::MAX;
}

/// Colour planes to interleaved RGB, or gray for a monochrome picture.
fn to_rgb(pic: &Picture) -> Result<(ColorType, Samples)> {
    let (width, height) = (pic.width(), pic.height());
    let layout = pic.pixel_layout();
    let range = match pic.color_range() {
        pixel::YUVRange::Limited => YuvRange::Limited,
        pixel::YUVRange::Full => YuvRange::Full,
    };
    let bits = bits_per_component(pic)?;
    let mono = layout == PixelLayout::I400;
    let color = if mono {
        ColorType::Gray
    } else {
        ColorType::Rgb
    };
    let out_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(color.channels()))
        .ok_or_else(|| Error::Codec("output buffer size overflow".into()))?;

    // GBR-coded RGB needs no matrix, just a plane shuffle.
    if pic.matrix_coefficients() == pixel::MatrixCoefficients::Identity && !mono {
        if layout != PixelLayout::I444 {
            return Err(Error::Codec(
                "identity matrix with subsampled chroma".into(),
            ));
        }
        let stride = pic.stride(PlanarImageComponent::Y);
        let samples = if bits == 8 {
            let (g, b, r) = (
                plane8(pic, PlanarImageComponent::Y),
                plane8(pic, PlanarImageComponent::U),
                plane8(pic, PlanarImageComponent::V),
            );
            Samples::U8(shuffle_gbr(&g, &b, &r, width, height, stride))
        } else {
            let (g, b, r) = (
                plane16(pic, PlanarImageComponent::Y),
                plane16(pic, PlanarImageComponent::U),
                plane16(pic, PlanarImageComponent::V),
            );
            let mut v = shuffle_gbr(&g, &b, &r, width, height, stride / 2);
            widen(&mut v, bits);
            Samples::U16(v)
        };
        return Ok((color, samples));
    }

    let matrix = matrix(pic.matrix_coefficients())?;
    let planes = Planes {
        layout,
        mono,
        range,
        matrix,
        bits,
    };
    // Mono goes through the RGB path too, so limited-range luma is expanded
    // by the backend rather than here; one channel is kept afterwards.
    let samples = if bits == 8 {
        Samples::U8(keep_channels(planes.convert8(pic)?, color.channels()))
    } else {
        Samples::U16(keep_channels(planes.convert16(pic)?, color.channels()))
    };
    debug_assert_eq!(samples.len(), out_len);
    Ok((color, samples))
}

/// What a picture's planes are, as far as conversion needs to know.
struct Planes {
    layout: PixelLayout,
    mono: bool,
    range: YuvRange,
    matrix: YuvStandardMatrix,
    bits: u32,
}

impl Planes {
    /// 8-bit planes to interleaved RGB, gray replicated across channels.
    fn convert8(&self, pic: &Picture) -> Result<Vec<u8>> {
        let (width, height) = (pic.width(), pic.height());
        let mut rgb = vec![0u8; width as usize * height as usize * 3];
        let y = plane8(pic, PlanarImageComponent::Y);
        if self.mono {
            let gray = YuvGrayImage {
                y_plane: &y,
                y_stride: pic.stride(PlanarImageComponent::Y),
                width,
                height,
            };
            yuv::yuv400_to_rgb(&gray, &mut rgb, width * 3, self.range, self.matrix)
                .map_err(codec_err)?;
            return Ok(rgb);
        }
        let (u, v) = (
            plane8(pic, PlanarImageComponent::U),
            plane8(pic, PlanarImageComponent::V),
        );
        let img = YuvPlanarImage {
            y_plane: &y,
            y_stride: pic.stride(PlanarImageComponent::Y),
            u_plane: &u,
            u_stride: pic.stride(PlanarImageComponent::U),
            v_plane: &v,
            v_stride: pic.stride(PlanarImageComponent::V),
            width,
            height,
        };
        let convert = match self.layout {
            PixelLayout::I420 => yuv::yuv420_to_rgb,
            PixelLayout::I422 => yuv::yuv422_to_rgb,
            PixelLayout::I444 => yuv::yuv444_to_rgb,
            PixelLayout::I400 => unreachable!("mono handled above"),
        };
        convert(&img, &mut rgb, width * 3, self.range, self.matrix).map_err(codec_err)?;
        Ok(rgb)
    }

    /// 10 or 12-bit planes to interleaved 16-bit RGB.
    fn convert16(&self, pic: &Picture) -> Result<Vec<u16>> {
        let (width, height) = (pic.width(), pic.height());
        let mut rgb = vec![0u16; width as usize * height as usize * 3];
        let y = plane16(pic, PlanarImageComponent::Y);
        if self.mono {
            let gray = YuvGrayImage {
                y_plane: &y,
                y_stride: pic.stride(PlanarImageComponent::Y) / 2,
                width,
                height,
            };
            let convert = match self.bits {
                10 => yuv::y010_to_rgb10,
                _ => yuv::y012_to_rgb12,
            };
            convert(&gray, &mut rgb, width * 3, self.range, self.matrix).map_err(codec_err)?;
        } else {
            let (u, v) = (
                plane16(pic, PlanarImageComponent::U),
                plane16(pic, PlanarImageComponent::V),
            );
            let img = YuvPlanarImage {
                y_plane: &y,
                y_stride: pic.stride(PlanarImageComponent::Y) / 2,
                u_plane: &u,
                u_stride: pic.stride(PlanarImageComponent::U) / 2,
                v_plane: &v,
                v_stride: pic.stride(PlanarImageComponent::V) / 2,
                width,
                height,
            };
            let convert = match (self.layout, self.bits) {
                (PixelLayout::I420, 10) => yuv::i010_to_rgb10,
                (PixelLayout::I422, 10) => yuv::i210_to_rgb10,
                (PixelLayout::I444, 10) => yuv::i410_to_rgb10,
                (PixelLayout::I420, _) => yuv::i012_to_rgb12,
                (PixelLayout::I422, _) => yuv::i212_to_rgb12,
                (PixelLayout::I444, _) => yuv::i412_to_rgb12,
                (PixelLayout::I400, _) => unreachable!("mono handled above"),
            };
            convert(&img, &mut rgb, width * 3, self.range, self.matrix).map_err(codec_err)?;
        }
        widen(&mut rgb, self.bits);
        Ok(rgb)
    }
}

/// Keep the first `channels` of every RGB triple; identity for 3.
fn keep_channels<T: Copy>(rgb: Vec<T>, channels: usize) -> Vec<T> {
    if channels == 3 {
        return rgb;
    }
    rgb.as_chunks::<3>()
        .0
        .iter()
        .flat_map(|px| px[..channels].to_vec())
        .collect()
}

/// Bits per component: 8, 10 or 12. Anything else is not AV1. Planes are
/// stored in `u8` for 8 and in host-endian `u16` for the other two.
fn bits_per_component(pic: &Picture) -> Result<u32> {
    match pic.bit_depth() {
        bits @ (8 | 10 | 12) => Ok(u32::try_from(bits).expect("small")),
        other => Err(Error::Codec(format!("unsupported AV1 bit depth {other}"))),
    }
}

fn matrix(mc: pixel::MatrixCoefficients) -> Result<YuvStandardMatrix> {
    use pixel::MatrixCoefficients as M;
    Ok(match mc {
        M::BT709 => YuvStandardMatrix::Bt709,
        // libavif treats "unspecified" as BT.601, so does everyone else.
        M::Unspecified | M::BT470BG | M::ST170M => YuvStandardMatrix::Bt601,
        M::BT470M => YuvStandardMatrix::Bt470_6,
        M::ST240M => YuvStandardMatrix::Smpte240,
        M::BT2020NonConstantLuminance => YuvStandardMatrix::Bt2020,
        other => {
            return Err(Error::Codec(format!(
                "unsupported AV1 matrix coefficients {other:?}"
            )));
        }
    })
}

/// A plane's bytes, 8-bit storage.
fn plane8(pic: &Picture, c: PlanarImageComponent) -> Vec<u8> {
    pic.plane(c).to_vec()
}

/// A plane's samples, 16-bit storage, host endian as the decoder wrote them.
fn plane16(pic: &Picture, c: PlanarImageComponent) -> Vec<u16> {
    pic.plane(c)
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| u16::from_ne_bytes(b))
        .collect()
}

/// Interleave three equal-stride planes as R, G, B from G, B, R planes.
fn shuffle_gbr<T: Copy>(
    green: &[T],
    blue: &[T],
    red: &[T],
    width: u32,
    height: u32,
    stride: u32,
) -> Vec<T> {
    let (width, height, stride) = (width as usize, height as usize, stride as usize);
    let mut out = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        for col in 0..width {
            let at = row * stride + col;
            out.extend_from_slice(&[red[at], green[at], blue[at]]);
        }
    }
    out
}

/// Scale `bits`-wide samples to the full 16-bit range by bit replication,
/// so that the maximum maps to `0xFFFF` exactly.
fn widen(v: &mut [u16], bits: u32) {
    match bits {
        10 => v.iter_mut().for_each(|s| *s = (*s << 6) | (*s >> 4)),
        12 => v.iter_mut().for_each(|s| *s = (*s << 4) | (*s >> 8)),
        _ => {}
    }
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
        b.extend_from_slice(&[0; 4]);
        for c in compatible {
            b.extend_from_slice(*c);
        }
        b.extend_from_slice(b"trailing bytes of the next box");
        b
    }

    #[test]
    fn probe_reads_brands() {
        let still = AvifDecoder
            .probe(&ftyp(*b"avif", &[b"mif1", b"miaf"]))
            .unwrap();
        assert_eq!((still.format, still.animated), (Format::Avif, false));
        let seq = AvifDecoder.probe(&ftyp(*b"avis", &[b"avif"])).unwrap();
        assert!(seq.animated);
        let compat = AvifDecoder.probe(&ftyp(*b"mif1", &[b"avif"])).unwrap();
        assert!(!compat.animated);
        assert!(AvifDecoder.probe(&ftyp(*b"heic", &[b"mif1"])).is_none());
        assert!(AvifDecoder.probe(b"\0\0\0\x0cftyp").is_none());
    }

    #[test]
    fn unpremultiply_recovers_colour() {
        // 50% alpha, premultiplied colour 100 -> straight 199 (rounded).
        assert_eq!(interleave(&[100u8], &[128u8], 1, true), vec![199, 128]);
        assert_eq!(interleave(&[100u8], &[128u8], 1, false), vec![100, 128]);
        // Opaque and fully transparent pixels are left alone.
        assert_eq!(interleave(&[7u8, 9], &[255u8], 2, true), vec![7, 9, 255]);
        assert_eq!(interleave(&[7u8, 9], &[0u8], 2, true), vec![7, 9, 0]);
        assert_eq!(
            interleave(&[0x4000u16], &[0x8000u16], 1, true),
            vec![0x8000, 0x8000]
        );
    }

    #[test]
    fn widen_hits_the_endpoints() {
        let mut v = [0u16, 1023];
        widen(&mut v, 10);
        assert_eq!(v, [0, 0xFFFF]);
        let mut v = [4095u16];
        widen(&mut v, 12);
        assert_eq!(v, [0xFFFF]);
    }

    #[test]
    fn gbr_shuffle_interleaves_rgb() {
        let out = shuffle_gbr(&[1u8, 2, 0], &[3, 4, 0], &[5, 6, 0], 2, 1, 3);
        assert_eq!(out, vec![5, 1, 3, 6, 2, 4]);
    }

    #[test]
    fn keep_channels_takes_the_first() {
        assert_eq!(keep_channels(vec![1u8, 2, 3, 4, 5, 6], 1), vec![1, 4]);
        assert_eq!(keep_channels(vec![1u8, 2, 3], 3), vec![1, 2, 3]);
    }
}
