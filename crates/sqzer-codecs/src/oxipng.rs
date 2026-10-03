//! PNG via `oxipng` (MIT): a filter and colour-type search over the raw
//! pixels, with `libdeflate` or `zopfli` for the DEFLATE step. On request,
//! `quantizr` (MIT) reduces the image to a palette first (ADR-0012).
//!
//! On every target. `libdeflate` is a vendored C library, the one C
//! dependency of the portable tier (ADR-0002); on wasm32 it is built in its
//! `freestanding` mode, which needs `clang` and no libc (ADR-0011 D1).

use std::collections::HashSet;

use oxipng::{
    BitDepth, ColorType as PngColor, Deflater, Options, RGBA8, RawImage, StripChunks, ZopfliOptions,
};
use sqzer_core::codec::{CodecOption, Encoder, EncoderCaps, Format, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::EncodeParams;
use sqzer_core::{Error, Result};

use crate::opts::{parse_bool, parse_percent, unknown};

/// PNG encoder: lossless unless asked for a palette, 8 or 16 bit, any
/// channel layout.
///
/// The stored layout may be narrower than the input when that loses
/// nothing: RGB with equal channels is written as grayscale, few colours
/// become a palette, 16-bit samples whose bytes repeat become 8-bit, and an
/// alpha channel that is opaque everywhere is dropped. Every reader expands
/// these back, so the pixels round-trip exactly while the layout may not.
///
/// Effort maps to `oxipng`'s presets `0..=6`; effort 10 adds `zopfli`,
/// which is several times slower for a few percent smaller output.
///
/// Options, all `png:` prefixed:
/// - `interlace`: write Adam7 interlaced output (default `false`).
/// - `optimize_alpha`: let fully transparent pixels take whatever colour
///   compresses best (default `false`, since it changes stored samples).
/// - `colors`: reduce the image to a palette of at most this many colours,
///   `2..=256` (default `off`). An image that already has that few is
///   written exactly, at its own depth; any other is quantised from 8-bit
///   RGBA, so 16-bit samples are narrowed.
/// - `dither`: how much of the quantisation error is diffused to
///   neighbouring pixels, `0..=100` (default `100`). Needs `colors`.
///
/// `colors` and `optimize_alpha` give samples up, so [`Encoder::exact`] is
/// `false` under either.
#[derive(Debug, Clone, Copy, Default)]
pub struct OxipngEncoder;

static CAPS: EncoderCaps = EncoderCaps {
    format: Format::Png,
    name: "oxipng",
    lossy: false,
    lossless: true,
    alpha: true,
    animation: false,
    bit_depth: &[8, 16],
    hdr: false,
    exif: true,
    xmp: true,
    quality_range: 100.0..=100.0,
    effort_range: 0..=10,
    tier: Tier::Portable,
    options: &[
        CodecOption {
            key: "interlace",
            default: "false",
            help: "write Adam7 interlaced output",
        },
        CodecOption {
            key: "optimize_alpha",
            default: "false",
            help: "let fully transparent pixels take whatever colour compresses best; lossy for those pixels",
        },
        CodecOption {
            key: "colors",
            default: "off",
            help: "reduce to a palette of at most `2..=256` colours; lossy",
        },
        CodecOption {
            key: "dither",
            default: "100",
            help: "error diffusion `0..=100` for `colors`, `0` for none",
        },
    ],
};

impl Encoder for OxipngEncoder {
    fn caps(&self) -> &EncoderCaps {
        &CAPS
    }

    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>> {
        // PNG is lossless whatever the target says; a quality only has to be
        // resolved, it is not used.
        params.resolved()?;
        let mut opts = map_effort(params.effort);
        let mut colors = None;
        let mut dither = None;
        for (key, value) in params.codec_opts("png") {
            match key {
                "interlace" => opts.interlace = Some(parse_bool("png", key, value)?),
                "optimize_alpha" => opts.optimize_alpha = parse_bool("png", key, value)?,
                "colors" => colors = parse_colors(value)?,
                "dither" => dither = Some(parse_percent("png", key, value)?),
                _ => return Err(unknown("png", key)),
            }
        }
        if dither.is_some() && colors.is_none() {
            return Err(Error::InvalidParams(
                "png:dither needs png:colors: there is nothing to dither without a palette".into(),
            ));
        }

        let mut raw = match colors {
            Some(max) if !within(img, max) => quantised(img, max, dither.unwrap_or(100))?,
            _ => as_is(img)?,
        };
        if let Some(icc) = img.icc() {
            raw.add_icc_profile(icc);
        }
        if let Some(exif) = img.exif() {
            raw.add_png_chunk(*b"eXIf", exif.to_vec());
        }
        if let Some(xmp) = img.xmp() {
            raw.add_png_chunk(*b"iTXt", itxt_xmp(&crate::png::xmp_text(xmp)?));
        }
        raw.create_optimized_png(&opts).map_err(codec_err)
    }

    fn exact(&self, params: &EncodeParams) -> bool {
        // A value `encode` refuses is not exact either: this only has to
        // never claim exactness for an option that may give samples up.
        params.codec_opts("png").all(|(key, value)| match key {
            "colors" => value == "off",
            "optimize_alpha" => matches!(parse_bool("png", key, value), Ok(false)),
            _ => true,
        })
    }
}

/// `off`, or a palette size `2..=256`.
fn parse_colors(value: &str) -> Result<Option<usize>> {
    if value == "off" {
        return Ok(None);
    }
    value
        .parse::<usize>()
        .ok()
        .filter(|n| (2..=256).contains(n))
        .map(Some)
        .ok_or_else(|| {
            Error::InvalidParams(format!(
                "png:colors expects `off` or an integer 2..=256, got `{value}`"
            ))
        })
}

/// Whether `img` has at most `max` distinct colours, counted on its samples
/// as they are: a 16-bit image is counted at 16 bits. Such an image is
/// written exactly (ADR-0012 D3).
fn within(img: &Image, max: usize) -> bool {
    fn count<T: Copy + Into<u64>>(samples: &[T], channels: usize, max: usize) -> bool {
        let mut seen = HashSet::with_capacity(max + 1);
        samples.chunks_exact(channels).all(|pixel| {
            seen.insert(pixel.iter().fold(0u64, |acc, &s| (acc << 16) | s.into()));
            seen.len() <= max
        })
    }
    let channels = img.color().channels();
    match img.samples() {
        Samples::U8(v) => count(v, channels, max),
        Samples::U16(v) => count(v, channels, max),
        // Float input is refused further down, whatever it holds.
        Samples::F32(_) => true,
    }
}

/// `img` reduced to a palette of at most `max` colours by `quantizr`, as an
/// indexed image for `oxipng`. `dither` is `0..=100`.
fn quantised(img: &Image, max: usize, dither: u8) -> Result<RawImage> {
    let narrow = img.to_u8(Format::Png)?;
    let samples = narrow.samples().as_u8().unwrap_or_default();
    let rgba: Vec<u8> = match narrow.color() {
        ColorType::Gray => samples.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        ColorType::GrayAlpha => samples
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[g, a]| [g, g, g, a])
            .collect(),
        ColorType::Rgb => samples
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        ColorType::Rgba => samples.to_vec(),
    };
    let (width, height) = (img.width() as usize, img.height() as usize);
    let source = quantizr::Image::new(&rgba, width, height).map_err(codec_err)?;
    let mut options = quantizr::Options::default();
    options
        .set_max_colors(i32::try_from(max).unwrap_or(256))
        .map_err(codec_err)?;
    let mut result = quantizr::QuantizeResult::quantize(&source, &options);
    result
        .set_dithering_level(f32::from(dither) / 100.0)
        .map_err(codec_err)?;
    let mut indices = vec![0u8; width * height];
    result
        .remap_image(&source, &mut indices)
        .map_err(codec_err)?;
    let palette = result.get_palette();
    let palette = palette.entries[..palette.count as usize]
        .iter()
        .map(|c| RGBA8::new(c.r, c.g, c.b, c.a))
        .collect();
    RawImage::new(
        img.width(),
        img.height(),
        PngColor::Indexed { palette },
        BitDepth::Eight,
        indices,
    )
    .map_err(codec_err)
}

/// `img` as it is, in its own layout and depth.
fn as_is(img: &Image) -> Result<RawImage> {
    let color = match img.color() {
        ColorType::Gray => PngColor::Grayscale {
            transparent_shade: None,
        },
        ColorType::GrayAlpha => PngColor::GrayscaleAlpha,
        ColorType::Rgb => PngColor::RGB {
            transparent_color: None,
        },
        ColorType::Rgba => PngColor::RGBA,
    };
    let (depth, bytes) = match img.samples() {
        Samples::U8(v) => (BitDepth::Eight, v.clone()),
        Samples::U16(v) => (
            BitDepth::Sixteen,
            v.iter().flat_map(|s| s.to_be_bytes()).collect(),
        ),
        Samples::F32(_) => {
            return Err(Error::Unsupported {
                format: Format::Png,
                what: "float (HDR) samples".into(),
            });
        }
    };

    RawImage::new(img.width(), img.height(), color, depth, bytes).map_err(codec_err)
}

/// The body of an uncompressed `iTXt` chunk carrying XMP: keyword, NUL,
/// compression flag and method, empty language tag and translated keyword,
/// then the packet.
fn itxt_xmp(xmp: &str) -> Vec<u8> {
    let mut body = crate::png::XMP_KEYWORD.as_bytes().to_vec();
    body.extend_from_slice(&[0, 0, 0, 0, 0]);
    body.extend_from_slice(xmp.as_bytes());
    body
}

/// Effort `0..=10` to an `oxipng` preset. The default effort, 6, lands on
/// preset 4: every filter strategy plus a short brute-force pass, at
/// `libdeflate`'s top level. Presets 5 and 6 only add longer brute-force
/// passes; `zopfli` at effort 10 is where the last few percent come from.
fn map_effort(effort: u8) -> Options {
    let preset = match effort {
        0 => 0,
        1 => 1,
        2 | 3 => 2,
        4 | 5 => 3,
        6 | 7 => 4,
        8 => 5,
        _ => 6,
    };
    let mut opts = Options::from_preset(preset);
    if effort >= 10 {
        opts.deflater = Deflater::Zopfli(ZopfliOptions::default());
    }
    // The only ancillary chunk this encoder adds is `iCCP`, and it is added
    // on purpose, so nothing needs stripping.
    opts.strip = StripChunks::None;
    opts
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_ladder_is_monotonic() {
        let filters: Vec<usize> = (0..=10).map(|e| map_effort(e).filters.len()).collect();
        assert!(filters.windows(2).all(|w| w[0] <= w[1]), "{filters:?}");
        assert!(matches!(
            map_effort(9).deflater,
            Deflater::Libdeflater { .. }
        ));
        assert!(matches!(map_effort(10).deflater, Deflater::Zopfli(_)));
    }
}
