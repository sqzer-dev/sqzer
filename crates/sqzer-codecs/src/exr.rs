//! `OpenEXR` via `exr` (BSD-3-Clause). Decoder only, to `f32` linear light,
//! which is what the format stores: half and float samples come out as
//! `f32`, an `R`, `G`, `B` layer as RGB with an `A` channel when there is
//! one, a `Y` layer as gray. The first valid layer of a multi-layer file is
//! read. Alpha is read as stored: the `OpenEXR` convention is associated
//! (premultiplied) alpha, but the tools that write EXR from 8-bit sources
//! do not premultiply, and there is no flag in the file that says which.
//!
//! A `chromaticities` attribute other than Rec.709 (the format's default,
//! and sRGB's primaries) is honoured for RGB layers by attaching a
//! matrix-shaper ICC profile with those primaries and white, synthesised
//! with `moxcms`. The colour stage of the pipeline then rotates the
//! samples to sRGB primaries in linear light, the same path every other
//! profiled input takes; values outside sRGB's gamut come out negative or
//! above one there, which the range stage clips. Malformed chromaticities
//! are an error, not a fallback to Rec.709.
//!
//! Not handled: deep data, resolution levels other than the largest,
//! channels that are neither `RGB(A)` nor `Y(A)`, subsampled channels and
//! the pixel aspect ratio. The `rayon` feature of `exr` is off:
//! parallelism is per file in the CLI (ADR-0001 D3).

use std::io::Cursor;

use exr::meta::attribute::Chromaticities;
use exr::prelude::*;
use moxcms::{Chromaticity, ColorPrimaries, ColorProfile};
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

/// What the first header says, before any pixel is read.
struct Header {
    width: u32,
    height: u32,
    channels: Channels,
    /// A profile for the layer's primaries when they are not sRGB's.
    icc: Option<Vec<u8>>,
}

/// CIE xy of Rec.709 / sRGB, the `OpenEXR` default.
const REC709: [(f32, f32); 4] = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06), (0.3127, 0.3290)];

/// An ICC profile with `chroma`'s primaries and white, or `None` when
/// `chroma` is Rec.709 already and the samples need no rotation.
///
/// # Errors
/// [`Error::Codec`] for chromaticities that are not finite or whose `y` is
/// not positive: no primaries can be built from them.
fn primaries_profile(chroma: &Chromaticities) -> Result<Option<Vec<u8>>> {
    let xy = |v: Vec2<f32>| (v.0, v.1);
    let points = [
        xy(chroma.red),
        xy(chroma.green),
        xy(chroma.blue),
        xy(chroma.white),
    ];
    if points
        .iter()
        .any(|(x, y)| !x.is_finite() || !y.is_finite() || *y <= 0.0)
    {
        return Err(Error::Codec(
            "OpenEXR chromaticities attribute is malformed".into(),
        ));
    }
    let same = points
        .iter()
        .zip(REC709)
        .all(|(a, b)| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4);
    if same {
        return Ok(None);
    }
    let point = |(x, y): (f32, f32)| Chromaticity::new(x, y);
    let mut profile = ColorProfile::new_srgb();
    profile.update_rgb_colorimetry(
        point(points[3]).to_xyyb(),
        ColorPrimaries {
            red: point(points[0]),
            green: point(points[1]),
            blue: point(points[2]),
        },
    );
    // Three primaries on a line, or two the same, give colorants that
    // cannot be inverted: the colour stage would divide by zero.
    let m = profile.colorant_matrix().v;
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !det.is_finite() || det.abs() < 1e-6 {
        return Err(Error::Codec(
            "OpenEXR chromaticities attribute is degenerate".into(),
        ));
    }
    profile.encode().map(Some).map_err(codec_err)
}

impl ExrDecoder {
    /// The first header's size, channel layout and a profile for its
    /// primaries when they are not sRGB's, without reading pixels.
    fn header(bytes: &[u8]) -> Result<Header> {
        let meta = MetaData::read_from_buffered(Cursor::new(bytes), false).map_err(codec_err)?;
        let header = meta
            .headers
            .first()
            .ok_or_else(|| Error::Codec("OpenEXR file has no layer".into()))?;
        if header.deep {
            return Err(Error::Codec("deep OpenEXR data is not supported".into()));
        }
        let width = u32::try_from(header.layer_size.0).map_err(|_| too_wide())?;
        let height = u32::try_from(header.layer_size.1).map_err(|_| too_wide())?;
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
        let icc = match (&channels, &header.shared_attributes.chromaticities) {
            (Channels::Rgb { .. }, Some(chroma)) => primaries_profile(chroma)?,
            _ => None,
        };
        Ok(Header {
            width,
            height,
            channels,
            icc,
        })
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
        Self::header(bytes).ok().map(|h| (h.width, h.height))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let Header {
            width,
            height,
            channels,
            icc,
        } = Self::header(bytes)?;
        opts.check_pixels(width, height)?;
        let (w, h) = (width as usize, height as usize);
        // The pixel guard bounds `w * h`; the sample count still has to fit
        // `usize`, which on a 32-bit target it need not.
        let samples_for = |ch: usize| {
            w.checked_mul(h)
                .and_then(|px| px.checked_mul(ch))
                .ok_or_else(|| Error::Codec("OpenEXR image too large for this platform".into()))
        };
        let (color, samples) = match channels {
            Channels::Rgb { alpha } => {
                let ch = if alpha { 4 } else { 3 };
                let len = samples_for(ch)?;
                let image = read()
                    .no_deep_data()
                    .largest_resolution_level()
                    .rgba_channels(
                        move |_, _| vec![0f32; len],
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
                let len = samples_for(ch)?;
                let image = read()
                    .no_deep_data()
                    .largest_resolution_level()
                    .specific_channels()
                    .required("Y")
                    .optional("A", 1.0f32)
                    .collect_pixels(
                        move |_, _| vec![0f32; len],
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
        Ok(Image::new(width, height, color, Samples::F32(samples))?.with_icc(icc))
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

    fn chroma(p: [(f32, f32); 4]) -> Chromaticities {
        Chromaticities {
            red: Vec2(p[0].0, p[0].1),
            green: Vec2(p[1].0, p[1].1),
            blue: Vec2(p[2].0, p[2].1),
            white: Vec2(p[3].0, p[3].1),
        }
    }

    #[test]
    fn rec709_needs_no_profile_and_p3_matches_moxcms_own() {
        assert!(
            primaries_profile(&chroma([
                (0.64, 0.33),
                (0.30, 0.60),
                (0.15, 0.06),
                (0.3127, 0.3290)
            ]))
            .unwrap()
            .is_none()
        );
        // Display P3 from its chromaticities lands on the colorants moxcms
        // ships for Display P3.
        let bytes = primaries_profile(&chroma([
            (0.680, 0.320),
            (0.265, 0.690),
            (0.150, 0.060),
            (0.3127, 0.3290),
        ]))
        .unwrap()
        .unwrap();
        let ours = ColorProfile::new_from_slice(&bytes)
            .unwrap()
            .colorant_matrix();
        let theirs = ColorProfile::new_display_p3().colorant_matrix();
        for (a, b) in ours.v.iter().flatten().zip(theirs.v.iter().flatten()) {
            assert!((a - b).abs() < 1e-3, "{ours:?} vs {theirs:?}");
        }
        // Malformed or degenerate chromaticities are errors, never Rec.709.
        for bad in [
            [(0.64, 0.0), (0.30, 0.60), (0.15, 0.06), (0.3127, 0.3290)],
            [
                (f32::NAN, 0.33),
                (0.30, 0.60),
                (0.15, 0.06),
                (0.3127, 0.3290),
            ],
            [(0.3, 0.3), (0.3, 0.3), (0.3, 0.3), (0.3, 0.3)],
        ] {
            assert!(
                matches!(primaries_profile(&chroma(bad)), Err(Error::Codec(_))),
                "{bad:?}"
            );
        }
    }

    /// An EXR written by `exr` itself with Display P3 chromaticities comes
    /// back with a profile that the colour stage turns into: pure P3 red
    /// is more red than sRGB can show and negative in green, a neutral
    /// stays neutral.
    #[test]
    fn a_p3_tagged_file_carries_its_primaries_as_a_profile() {
        let mut image = exr::image::Image::from_layer(Layer::new(
            (2, 1),
            LayerAttributes::named("main"),
            Encoding::UNCOMPRESSED,
            SpecificChannels::rgb(|Vec2(x, _)| {
                if x == 0 {
                    (1.0f32, 0.0f32, 0.0f32)
                } else {
                    (0.5f32, 0.5f32, 0.5f32)
                }
            }),
        ));
        image.attributes.chromaticities = Some(chroma([
            (0.680, 0.320),
            (0.265, 0.690),
            (0.150, 0.060),
            (0.3127, 0.3290),
        ]));
        let mut bytes = Cursor::new(Vec::new());
        image
            .write()
            .non_parallel()
            .to_buffered(&mut bytes)
            .unwrap();
        let img = ExrDecoder
            .decode(bytes.get_ref(), &DecodeOpts::default())
            .unwrap();
        let profile = ColorProfile::new_from_slice(img.icc().expect("profile")).unwrap();
        // The linear matrix the colour stage applies to float samples.
        let m = profile.transform_matrix(&ColorProfile::new_srgb()).v;
        let v = img.samples().as_f32().unwrap();
        let red = [
            m[0][0] * f64::from(v[0]) + m[0][1] * f64::from(v[1]) + m[0][2] * f64::from(v[2]),
            m[1][0] * f64::from(v[0]) + m[1][1] * f64::from(v[1]) + m[1][2] * f64::from(v[2]),
            m[2][0] * f64::from(v[0]) + m[2][1] * f64::from(v[1]) + m[2][2] * f64::from(v[2]),
        ];
        assert!(
            red[0] > 1.2 && red[1] < -0.03 && red[2].abs() < 0.03,
            "{red:?}"
        );
        let gray: f64 = m
            .iter()
            .map(|row| row.iter().sum::<f64>() * 0.5)
            .sum::<f64>()
            / 3.0;
        assert!((gray - 0.5).abs() < 1e-3, "{gray}");
        // Untagged fixtures carry none.
        assert_eq!(v.len(), 6);
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
