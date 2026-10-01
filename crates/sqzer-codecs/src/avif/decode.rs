//! AVIF decoding: `avif-parse` (MPL-2.0) splits the container, `rav1d`
//! (BSD-2) decodes the AV1 payloads, `yuv` (BSD-3/Apache) converts to RGB.
//!
//! On every target, wasm32 included. The crate is `sqzer-rav1d`, upstream
//! `rav1d` `main` with its safe Rust API and a stand-in for the `libc`
//! names wasm32 lacks (ADR-0011 D2).
//!
//! `avif-parse` does not surface the `colr` box, `irot` and `imir` or the
//! Exif and XMP items, so those come from the HEIF container walk in
//! [`crate::heif`] that the HEIC backends share:
//! - `irot` and `imir` are applied, always, as for HEIC: they are part of
//!   the picture's geometry, not a hint like an EXIF tag.
//! - An ICC profile from `colr` is attached. Without one, primaries and a
//!   transfer curve other than sRGB, from an `nclx` box or else the AV1
//!   sequence header, become an ICC profile built with `moxcms`, so the
//!   colour stage converts a Display P3 or BT.2020 file like a tagged one.
//!   PQ and HLG get no profile: there is no tone mapping (ADR-0007).
//! - The Exif and XMP items are attached, the Exif orientation tag reset.
//!
//! Known limits of this backend:
//! - A `clap` crop is not applied; the full coded frame is returned.
//! - Animated files (`avis`) yield their first frame.
//! - Decoding is single-threaded on the pure-Rust paths; assembly is
//!   compiled out so no `nasm` is needed.

use std::io::Cursor;

use rav1d::{
    Decoder as Av1Decoder, Picture, PixelLayout, PlanarImageComponent, Rav1dError as Av1Error,
    Settings, pixel,
};
use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Orientation, Samples};
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
    name: "rav1d",
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

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        let data = avif_parse::read_avif(&mut Cursor::new(bytes)).ok()?;
        let meta = data.primary_item_metadata().ok()?;
        let (w, h) = (meta.max_frame_width.get(), meta.max_frame_height.get());
        // The displayed size: a quarter turn swaps the axes.
        let turned = crate::heif::read(bytes).is_some_and(|h| h.orientation.swaps_axes());
        Some(if turned { (h, w) } else { (w, h) })
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let data = avif_parse::read_avif(&mut Cursor::new(bytes)).map_err(codec_err)?;
        let meta = data.primary_item_metadata().map_err(codec_err)?;
        opts.check_pixels(meta.max_frame_width.get(), meta.max_frame_height.get())?;

        let color = decode_av1(&data.primary_item)?;
        let (width, height) = (color.width(), color.height());
        opts.check_pixels(width, height)?;
        let bitstream = (
            primaries_code(color.color_primaries()),
            transfer_code(color.transfer_characteristic()),
        );
        let (color_type, samples) = to_rgb(&color)?;

        let Some(alpha_obu) = &data.alpha_item else {
            return finish(
                Image::new(width, height, color_type, samples)?,
                bytes,
                bitstream,
            );
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
        finish(
            Image::new(width, height, with_alpha, samples)?,
            bytes,
            bitstream,
        )
    }
}

/// What the container adds to the decoded picture: its metadata, a
/// profile for primaries other than sRGB, and its rotation and mirroring.
/// `bitstream` is the AV1 sequence header's primaries and transfer, the
/// fallback when the container has no `colr` box.
fn finish(image: Image, bytes: &[u8], bitstream: (u16, u16)) -> Result<Image> {
    let header = crate::heif::read(bytes);
    let mut meta = header
        .as_ref()
        .map(crate::heif::Header::metadata)
        .unwrap_or_default();
    if meta.icc.is_none() && matches!(image.color(), ColorType::Rgb | ColorType::Rgba) {
        let (primaries, transfer) = header
            .as_ref()
            .and_then(|h| h.nclx)
            .map_or(bitstream, |n| (n.primaries, n.transfer));
        meta.icc = cicp_profile(primaries, transfer)?;
    }
    let orientation = header.map_or(Orientation::Normal, |h| h.orientation);
    Ok(image.with_metadata(meta).apply_orientation(orientation))
}

/// An ICC profile for colour code points (ITU-T H.273) other than sRGB,
/// or `None` when sRGB describes them or no profile can: unspecified or
/// BT.709 primaries with an SDR camera or sRGB curve are sRGB, as every
/// browser treats them, and PQ, HLG and the log curves have no ICC curve.
fn cicp_profile(primaries: u16, transfer: u16) -> Result<Option<Vec<u8>>> {
    use moxcms::{
        CicpColorPrimaries, CicpProfile, ColorProfile, MatrixCoefficients, TransferCharacteristics,
    };
    // Primaries with chromaticities `moxcms` knows.
    const PRIMARIES: [u16; 10] = [1, 4, 5, 6, 7, 8, 9, 11, 12, 22];
    // SDR curves with an ICC equivalent; 2 (unspecified) reads as sRGB.
    const TRANSFERS: [u16; 9] = [1, 4, 5, 6, 7, 8, 13, 14, 15];
    // What sRGB already describes.
    const SRGB_LIKE: [u16; 5] = [1, 6, 13, 14, 15];

    let primaries = if primaries == 2 { 1 } else { primaries };
    let transfer = if transfer == 2 { 13 } else { transfer };
    if !PRIMARIES.contains(&primaries) || !TRANSFERS.contains(&transfer) {
        return Ok(None);
    }
    if primaries == 1 && SRGB_LIKE.contains(&transfer) {
        return Ok(None);
    }
    let code = |v: u16| u8::try_from(v).map_err(codec_err);
    let profile = ColorProfile::new_from_cicp(CicpProfile {
        color_primaries: CicpColorPrimaries::try_from(code(primaries)?).map_err(codec_err)?,
        transfer_characteristics: TransferCharacteristics::try_from(code(transfer)?)
            .map_err(codec_err)?,
        // Unused for an RGB profile.
        matrix_coefficients: MatrixCoefficients::Bt709,
        full_range: true,
    });
    if profile.red_trc.is_none() {
        return Ok(None);
    }
    profile.encode().map(Some).map_err(codec_err)
}

/// The H.273 code point of the sequence header's primaries.
fn primaries_code(p: pixel::ColorPrimaries) -> u16 {
    use pixel::ColorPrimaries as P;
    match p {
        P::BT709 => 1,
        P::BT470M => 4,
        P::BT470BG => 5,
        P::ST240M => 7,
        P::Film => 8,
        P::BT2020 => 9,
        P::ST428 => 10,
        P::P3DCI => 11,
        P::P3Display => 12,
        P::Tech3213 => 22,
        _ => 2,
    }
}

/// The H.273 code point of the sequence header's transfer curve.
fn transfer_code(t: pixel::TransferCharacteristic) -> u16 {
    use pixel::TransferCharacteristic as T;
    match t {
        T::BT1886 => 1,
        T::BT470M => 4,
        T::BT470BG => 5,
        T::ST170M => 6,
        T::ST240M => 7,
        T::Linear => 8,
        T::Logarithmic100 => 9,
        T::Logarithmic316 => 10,
        T::SRGB => 13,
        T::BT2020Ten => 14,
        T::BT2020Twelve => 15,
        T::PerceptualQuantizer => 16,
        T::ST428 => 17,
        T::HybridLogGamma => 18,
        _ => 2,
    }
}

/// Decode one still AV1 temporal unit to a picture.
fn decode_av1(obu: &[u8]) -> Result<Picture> {
    let mut settings = Settings::new();
    settings.set_n_threads(1);
    settings.set_max_frame_delay(1);
    let mut decoder = Av1Decoder::with_settings(&settings).map_err(codec_err)?;
    match decoder.send_data(obu.into(), None, None, None) {
        Ok(()) | Err(Av1Error::TryAgain) => {}
        Err(e) => return Err(codec_err(e)),
    }
    loop {
        match decoder.get_picture() {
            Ok(picture) => return Ok(picture),
            Err(Av1Error::TryAgain) => match decoder.send_pending_data() {
                Ok(()) => {
                    return decoder.get_picture().map_err(|e| match e {
                        Av1Error::TryAgain => Error::Codec("AV1 payload holds no picture".into()),
                        other => codec_err(other),
                    });
                }
                Err(Av1Error::TryAgain) => {}
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
    fn code_points_other_than_srgb_become_a_profile() {
        use moxcms::ColorProfile;
        // sRGB and what browsers read as sRGB: no profile.
        for (p, t) in [(1, 13), (1, 1), (1, 6), (2, 2), (1, 2), (2, 13)] {
            assert_eq!(cicp_profile(p, t).unwrap(), None, "{p}/{t}");
        }
        // PQ, HLG and log curves: no ICC equivalent, no tone mapping.
        for (p, t) in [(9, 16), (9, 18), (1, 9)] {
            assert_eq!(cicp_profile(p, t).unwrap(), None, "{p}/{t}");
        }
        // Display P3 with the sRGB curve: P3 colorants.
        let icc = cicp_profile(12, 13).unwrap().expect("P3 profile");
        let ours = ColorProfile::new_from_slice(&icc)
            .unwrap()
            .colorant_matrix();
        let p3 = ColorProfile::new_display_p3().colorant_matrix();
        for (a, b) in ours.v.iter().flatten().zip(p3.v.iter().flatten()) {
            assert!((a - b).abs() < 1e-3, "{:?} vs {:?}", ours.v, p3.v);
        }
        // sRGB primaries with a linear curve still need a profile.
        assert!(cicp_profile(1, 8).unwrap().is_some());
        // The sequence header's enums map back to their code points.
        assert_eq!(primaries_code(pixel::ColorPrimaries::P3Display), 12);
        assert_eq!(transfer_code(pixel::TransferCharacteristic::SRGB), 13);
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
