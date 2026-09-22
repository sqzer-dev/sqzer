//! PNG via `oxipng` (MIT): a filter and colour-type search over the raw
//! pixels, with `libdeflate` or `zopfli` for the DEFLATE step.
//!
//! Desktop only. `libdeflate` is a vendored C library, so this module is
//! compiled out on wasm32 and the plain writer in [`crate::png`] is
//! registered there instead. ADR-0002 records the exception.

use oxipng::{
    BitDepth, ColorType as PngColor, Deflater, Options, RawImage, StripChunks, ZopfliOptions,
};
use sqzer_core::codec::{CodecOption, Encoder, EncoderCaps, Format, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::EncodeParams;
use sqzer_core::{Error, Result};

use crate::opts::{parse_bool, unknown};

/// PNG encoder: lossless, 8 or 16 bit, any channel layout.
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
            help: "let fully transparent pixels take whatever colour compresses best",
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
        for (key, value) in params.codec_opts("png") {
            match key {
                "interlace" => opts.interlace = Some(parse_bool("png", key, value)?),
                "optimize_alpha" => opts.optimize_alpha = parse_bool("png", key, value)?,
                _ => return Err(unknown("png", key)),
            }
        }

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

        let mut raw =
            RawImage::new(img.width(), img.height(), color, depth, bytes).map_err(codec_err)?;
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
