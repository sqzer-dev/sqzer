//! `OpenEXR` via `exr` (BSD-3-Clause). Decoder only, to `f32` linear light,
//! which is what the format stores: half and float samples come out as
//! `f32`, an `R`, `G`, `B` layer as RGB with an `A` channel when there is
//! one, a `Y` layer as gray. The first valid layer of a multi-layer file is
//! read. Alpha is read as stored: the `OpenEXR` convention is associated
//! (premultiplied) alpha, but the tools that write EXR from 8-bit sources
//! do not premultiply, and there is no flag in the file that says which.
//!
//! Not handled: deep data, resolution levels other than the largest,
//! channels that are neither `RGB(A)` nor `Y(A)`, subsampled channels,
//! the `chromaticities` attribute (samples are read as Rec.709 primaries,
//! which is the `OpenEXR` default and sRGB's) and the pixel aspect ratio.
//! The `rayon` feature of `exr` is off: parallelism is per file in the
//! CLI (ADR-0001 D3).

use std::io::Cursor;

use exr::prelude::*;
use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};

/// `OpenEXR` magic number.
const MAGIC: [u8; 4] = [0x76, 0x2f, 0x31, 0x01];

static CAPS: DecoderCaps = DecoderCaps {
    format: Format::Exr,
    name: "exr",
    animation: false,
    tier: Tier::Portable,
};

/// `OpenEXR` decoder.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExrDecoder;

/// Which channels the first layer has, and so what the image becomes.
enum Channels {
    Rgb { alpha: bool },
    Gray { alpha: bool },
}

impl ExrDecoder {
    /// The first header's size and channel layout, without reading pixels.
    fn header(bytes: &[u8]) -> Result<((u32, u32), Channels)> {
        let meta = MetaData::read_from_buffered(Cursor::new(bytes), false).map_err(codec_err)?;
        let header = meta
            .headers
            .first()
            .ok_or_else(|| Error::Codec("OpenEXR file has no layer".into()))?;
        if header.deep {
            return Err(Error::Codec("deep OpenEXR data is not supported".into()));
        }
        let size = (
            u32::try_from(header.layer_size.0).map_err(|_| too_wide())?,
            u32::try_from(header.layer_size.1).map_err(|_| too_wide())?,
        );
        let has = |name: &str| header.channels.list.iter().any(|c| c.name == *name);
        let channels = if has("R") && has("G") && has("B") {
            Channels::Rgb { alpha: has("A") }
        } else if has("Y") {
            Channels::Gray { alpha: has("A") }
        } else {
            let names: Vec<String> = header
                .channels
                .list
                .iter()
                .map(|c| c.name.to_string())
                .collect();
            return Err(Error::Codec(format!(
                "OpenEXR channels {} are not supported; RGB, RGBA, Y and YA are",
                names.join(", ")
            )));
        };
        Ok((size, channels))
    }
}

fn too_wide() -> Error {
    Error::Codec("OpenEXR layer size does not fit in 32 bits".into())
}

impl Decoder for ExrDecoder {
    fn caps(&self) -> &DecoderCaps {
        &CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        bytes.starts_with(&MAGIC).then_some(FormatInfo {
            format: Format::Exr,
            animated: false,
        })
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        Self::header(bytes).ok().map(|(size, _)| size)
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let ((width, height), channels) = Self::header(bytes)?;
        opts.check_pixels(width, height)?;
        let (w, h) = (width as usize, height as usize);
        let (color, samples) = match channels {
            Channels::Rgb { alpha } => {
                let ch = if alpha { 4 } else { 3 };
                let image = read()
                    .no_deep_data()
                    .largest_resolution_level()
                    .rgba_channels(
                        move |_, _| vec![0f32; w * h * ch],
                        move |pixels: &mut Vec<f32>, at, (r, g, b, a): (f32, f32, f32, f32)| {
                            let i = (at.1 * w + at.0) * ch;
                            pixels[i] = r;
                            pixels[i + 1] = g;
                            pixels[i + 2] = b;
                            if alpha {
                                pixels[i + 3] = a;
                            }
                        },
                    )
                    .first_valid_layer()
                    .all_attributes()
                    .non_parallel()
                    .from_buffered(Cursor::new(bytes))
                    .map_err(codec_err)?;
                let color = if alpha {
                    ColorType::Rgba
                } else {
                    ColorType::Rgb
                };
                (color, image.layer_data.channel_data.pixels)
            }
            Channels::Gray { alpha } => {
                let ch = if alpha { 2 } else { 1 };
                let image = read()
                    .no_deep_data()
                    .largest_resolution_level()
                    .specific_channels()
                    .required("Y")
                    .optional("A", 1.0f32)
                    .collect_pixels(
                        move |_, _| vec![0f32; w * h * ch],
                        move |pixels: &mut Vec<f32>, at, (y, a): (f32, f32)| {
                            let i = (at.1 * w + at.0) * ch;
                            pixels[i] = y;
                            if alpha {
                                pixels[i + 1] = a;
                            }
                        },
                    )
                    .first_valid_layer()
                    .all_attributes()
                    .non_parallel()
                    .from_buffered(Cursor::new(bytes))
                    .map_err(codec_err)?;
                let color = if alpha {
                    ColorType::GrayAlpha
                } else {
                    ColorType::Gray
                };
                (color, image.layer_data.channel_data.pixels)
            }
        };
        Image::new(width, height, color, Samples::F32(samples))
    }
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_is_the_magic_number() {
        assert!(
            ExrDecoder
                .probe(&[0x76, 0x2f, 0x31, 0x01, 2, 0, 0, 0])
                .is_some()
        );
        assert!(ExrDecoder.probe(b"\x89PNG").is_none());
        assert!(ExrDecoder.probe(&[]).is_none());
        assert_eq!(ExrDecoder.dimensions(b"\x89PNG"), None);
    }

    #[test]
    fn a_truncated_file_is_an_error() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&[2, 0, 0, 0, 0]);
        assert!(matches!(
            ExrDecoder.decode(&bytes, &DecodeOpts::default()),
            Err(Error::Codec(_))
        ));
    }
}
