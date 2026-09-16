//! TIFF via the `tiff` crate (MIT). Decoder only, first image directory.
//! Gray, gray-alpha, RGB and RGBA at 8 or 16 bits, 32-bit float samples
//! as `f32`, and palette images expanded through their colour map. The
//! `Orientation` tag is applied and the ICC profile tag is kept. CMYK and
//! YCbCr need a colour transform this crate does not write and are
//! refused.

use std::io::Cursor;

use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Orientation, Samples};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};
use tiff::decoder::DecodingResult;
use tiff::tags::Tag;

/// TIFF decoder, first page only.
#[derive(Debug, Clone, Copy, Default)]
pub struct TiffDecoder;

static CAPS: DecoderCaps = DecoderCaps {
    format: Format::Tiff,
    name: "tiff",
    animation: false,
    tier: Tier::Portable,
};

/// `InterColorProfile`, the ICC tag.
const ICC_TAG: u16 = 34675;

impl Decoder for TiffDecoder {
    fn caps(&self) -> &DecoderCaps {
        &CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        // Classic and BigTIFF, both byte orders.
        let magic = bytes.get(..4)?;
        let ok = matches!(
            magic,
            b"II\x2A\x00" | b"MM\x00\x2A" | b"II\x2B\x00" | b"MM\x00\x2B"
        );
        ok.then_some(FormatInfo {
            format: Format::Tiff,
            animated: false,
        })
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        // The stored size, as every other decoder reports it; the
        // `Orientation` tag is applied by `decode` only.
        tiff::decoder::Decoder::new(Cursor::new(bytes))
            .ok()?
            .dimensions()
            .ok()
    }

    #[allow(clippy::too_many_lines)]
    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let mut d = tiff::decoder::Decoder::new(Cursor::new(bytes))
            .map_err(codec_err)?
            .with_limits(tiff::decoder::Limits::unlimited());
        let (width, height) = d.dimensions().map_err(codec_err)?;
        opts.check_pixels(width, height)?;

        let color_type = d.colortype().map_err(codec_err)?;
        let color = match color_type {
            tiff::ColorType::Gray(_) => ColorType::Gray,
            tiff::ColorType::GrayA(_) => ColorType::GrayAlpha,
            tiff::ColorType::RGB(_) | tiff::ColorType::Palette(_) => ColorType::Rgb,
            tiff::ColorType::RGBA(_) => ColorType::Rgba,
            other => {
                return Err(Error::Codec(format!(
                    "TIFF colour type {other:?} needs a colour transform, which is not supported"
                )));
            }
        };
        let icc = d
            .find_tag(Tag::Unknown(ICC_TAG))
            .ok()
            .flatten()
            .and_then(|v| v.into_u8_vec().ok());
        let orient = if opts.apply_orientation {
            orientation(&mut d)
        } else {
            Orientation::default()
        };

        let samples = match d.read_image().map_err(codec_err)? {
            DecodingResult::U8(v) if matches!(color_type, tiff::ColorType::Palette(_)) => {
                Samples::U16(expand_palette(&mut d, &v)?)
            }
            DecodingResult::U16(v) if matches!(color_type, tiff::ColorType::Palette(_)) => {
                let idx: Vec<u8> = v
                    .iter()
                    .map(|&i| u8::try_from(i).unwrap_or(u8::MAX))
                    .collect();
                Samples::U16(expand_palette(&mut d, &idx)?)
            }
            DecodingResult::U8(v) => Samples::U8(v),
            DecodingResult::U16(v) => Samples::U16(v),
            DecodingResult::F32(v) => Samples::F32(v),
            other => {
                return Err(Error::Codec(format!(
                    "TIFF {} samples are not supported",
                    sample_name(&other)
                )));
            }
        };
        Ok(Image::new(width, height, color, samples)?
            .with_icc(icc)
            .apply_orientation(orient))
    }
}

fn orientation<R: std::io::Read + std::io::Seek>(d: &mut tiff::decoder::Decoder<R>) -> Orientation {
    d.find_tag(Tag::Orientation)
        .ok()
        .flatten()
        .and_then(|v| v.into_u32().ok())
        .and_then(Orientation::from_exif)
        .unwrap_or_default()
}

/// Expand palette indices through the `ColorMap` tag: three planes of
/// 16-bit values, red then green then blue, one entry per index.
fn expand_palette<R: std::io::Read + std::io::Seek>(
    d: &mut tiff::decoder::Decoder<R>,
    indices: &[u8],
) -> Result<Vec<u16>> {
    let map = d
        .find_tag(Tag::ColorMap)
        .map_err(codec_err)?
        .ok_or_else(|| Error::Codec("palette TIFF without a colour map".into()))?
        .into_u16_vec()
        .map_err(codec_err)?;
    let entries = map.len() / 3;
    if entries == 0 {
        return Err(Error::Codec("empty TIFF colour map".into()));
    }
    let mut out = Vec::with_capacity(indices.len() * 3);
    for &i in indices {
        let i = usize::from(i).min(entries - 1);
        out.extend([map[i], map[entries + i], map[2 * entries + i]]);
    }
    Ok(out)
}

fn sample_name(r: &DecodingResult) -> &'static str {
    match r {
        DecodingResult::U8(_) => "u8",
        DecodingResult::U16(_) => "u16",
        DecodingResult::U32(_) => "u32",
        DecodingResult::U64(_) => "u64",
        DecodingResult::F16(_) => "f16",
        DecodingResult::F32(_) => "f32",
        DecodingResult::F64(_) => "f64",
        DecodingResult::I8(_) => "i8",
        DecodingResult::I16(_) => "i16",
        DecodingResult::I32(_) => "i32",
        DecodingResult::I64(_) => "i64",
    }
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_accepts_both_byte_orders_and_bigtiff() {
        for magic in [b"II\x2A\x00", b"MM\x00\x2A", b"II\x2B\x00", b"MM\x00\x2B"] {
            assert!(TiffDecoder.probe(magic).is_some());
        }
        assert!(TiffDecoder.probe(b"II\x00\x2A").is_none());
        assert!(TiffDecoder.probe(b"II").is_none());
        assert!(TiffDecoder.dimensions(b"II\x2A\x00").is_none());
    }
}
