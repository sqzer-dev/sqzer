//! `OpenEXR` via `exr` (BSD-3-Clause). Decoder only, to `f32` linear light,
//! which is what the format stores: half and float samples come out as
//! `f32`, an `R`, `G`, `B` layer as RGB with an `A` channel when there is
//! one, a `Y` layer as gray. The first valid layer of a multi-layer file is
//! read. Alpha is read as stored: the `OpenEXR` convention is associated
//! (premultiplied) alpha, but the tools that write EXR from 8-bit sources
//! do not premultiply, and there is no flag in the file that says which.
//!
//! A `chromaticities` attribute other than Rec.709 (the format's default,
//! and sRGB's primaries) is honoured for RGB layers: the samples are
//! rotated to sRGB primaries in linear light through one matrix, with a
//! Bradford adaptation when the white point is not D65. Values outside
//! sRGB's gamut come out negative or above one, which the range stage
//! clips.
//!
//! Not handled: deep data, resolution levels other than the largest,
//! channels that are neither `RGB(A)` nor `Y(A)`, subsampled channels and
//! the pixel aspect ratio. The `rayon` feature of `exr` is off:
//! parallelism is per file in the CLI (ADR-0001 D3).

use std::io::Cursor;

use exr::meta::attribute::Chromaticities;
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

/// A 3 x 3 matrix, row major.
type Matrix = [[f64; 3]; 3];

/// CIE xy of Rec.709 / sRGB, the `OpenEXR` default.
const REC709: [(f64, f64); 4] = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06), (0.3127, 0.3290)];

const IDENTITY: Matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The linear matrix taking RGB in `chroma`'s primaries and white to sRGB
/// primaries at D65, or `None` when `chroma` is Rec.709 already or is
/// degenerate.
fn to_srgb_matrix(chroma: &Chromaticities) -> Option<Matrix> {
    let xy = |v: Vec2<f32>| (f64::from(v.0), f64::from(v.1));
    let prim = [
        xy(chroma.red),
        xy(chroma.green),
        xy(chroma.blue),
        xy(chroma.white),
    ];
    let same = prim
        .iter()
        .zip(REC709)
        .all(|(a, b)| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4);
    if same {
        return None;
    }
    let src = rgb_to_xyz(prim)?;
    let adapt = bradford(prim[3], REC709[3]);
    let dst = invert(rgb_to_xyz(REC709)?)?;
    Some(mul(mul(dst, adapt), src))
}

/// RGB to XYZ for primaries and white given as CIE xy, the textbook
/// construction: the primaries' XYZ scaled so that RGB (1, 1, 1) is the
/// white. `None` for degenerate chromaticities.
fn rgb_to_xyz([red, green, blue, white]: [(f64, f64); 4]) -> Option<Matrix> {
    let xyz = |(x, y): (f64, f64)| {
        if y.abs() < 1e-9 {
            None
        } else {
            Some([x / y, 1.0, (1.0 - x - y) / y])
        }
    };
    let (red, green, blue, white) = (xyz(red)?, xyz(green)?, xyz(blue)?, xyz(white)?);
    let primaries = [
        [red[0], green[0], blue[0]],
        [red[1], green[1], blue[1]],
        [red[2], green[2], blue[2]],
    ];
    let scale = mul_vec(invert(primaries)?, white);
    let mut out = primaries;
    for row in &mut out {
        for (cell, k) in row.iter_mut().zip(scale) {
            *cell *= k;
        }
    }
    Some(out)
}

/// Bradford chromatic adaptation from white `from` to white `to`, both as
/// CIE xy. The identity when they agree.
fn bradford(from: (f64, f64), to: (f64, f64)) -> Matrix {
    const B: Matrix = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    const B_INV: Matrix = [
        [0.986_993, -0.147_054, 0.159_963],
        [0.432_305, 0.518_360, 0.049_291],
        [-0.008_529, 0.040_043, 0.968_487],
    ];
    if (from.0 - to.0).abs() < 1e-6 && (from.1 - to.1).abs() < 1e-6 {
        return IDENTITY;
    }
    let white = |(x, y): (f64, f64)| [x / y, 1.0, (1.0 - x - y) / y];
    let s = mul_vec(B, white(from));
    let d = mul_vec(B, white(to));
    let scale = [
        [d[0] / s[0], 0.0, 0.0],
        [0.0, d[1] / s[1], 0.0],
        [0.0, 0.0, d[2] / s[2]],
    ];
    mul(mul(B_INV, scale), B)
}

fn mul(a: Matrix, b: Matrix) -> Matrix {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn mul_vec(m: Matrix, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Inverse by cofactors; `None` when singular.
fn invert(m: Matrix) -> Option<Matrix> {
    let c = |r: usize, c: usize| -> f64 {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    if det.abs() < 1e-12 {
        return None;
    }
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            // Transposed cofactor over the determinant.
            *cell = c(j, i) / det;
        }
    }
    Some(out)
}

/// Rotate interleaved linear RGB(A) samples through `m`, alpha untouched.
fn apply(m: Matrix, samples: &mut [f32], channels: usize) {
    for px in samples.chunks_exact_mut(channels) {
        let v = mul_vec(m, [f64::from(px[0]), f64::from(px[1]), f64::from(px[2])]);
        #[allow(clippy::cast_possible_truncation)]
        for (out, s) in px.iter_mut().zip(v) {
            *out = s as f32;
        }
    }
}

impl ExrDecoder {
    /// The first header's size, channel layout and the matrix that brings
    /// its primaries to sRGB, without reading pixels.
    fn header(bytes: &[u8]) -> Result<((u32, u32), Channels, Option<Matrix>)> {
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
        let matrix = match channels {
            Channels::Rgb { .. } => header
                .shared_attributes
                .chromaticities
                .as_ref()
                .and_then(to_srgb_matrix),
            Channels::Gray { .. } => None,
        };
        Ok((size, channels, matrix))
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
        Self::header(bytes).ok().map(|(size, _, _)| size)
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let ((width, height), channels, matrix) = Self::header(bytes)?;
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
                let mut pixels = image.layer_data.channel_data.pixels;
                if let Some(m) = matrix {
                    apply(m, &mut pixels, ch);
                }
                (color, pixels)
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

    fn chroma(p: [(f32, f32); 4]) -> Chromaticities {
        Chromaticities {
            red: Vec2(p[0].0, p[0].1),
            green: Vec2(p[1].0, p[1].1),
            blue: Vec2(p[2].0, p[2].1),
            white: Vec2(p[3].0, p[3].1),
        }
    }

    #[test]
    fn rec709_needs_no_matrix_and_p3_matches_the_published_one() {
        assert!(
            to_srgb_matrix(&chroma([
                (0.64, 0.33),
                (0.30, 0.60),
                (0.15, 0.06),
                (0.3127, 0.3290)
            ]))
            .is_none()
        );
        // Display P3 to sRGB, both D65, as Lindbloom and colour-science
        // publish it.
        let m = to_srgb_matrix(&chroma([
            (0.680, 0.320),
            (0.265, 0.690),
            (0.150, 0.060),
            (0.3127, 0.3290),
        ]))
        .unwrap();
        let published: Matrix = [
            [1.2249, -0.2247, 0.0],
            [-0.0420, 1.0419, 0.0],
            [-0.0197, -0.0786, 1.0979],
        ];
        for (row, want) in m.iter().zip(published) {
            for (a, b) in row.iter().zip(want) {
                assert!((a - b).abs() < 2e-3, "{m:?}");
            }
        }
        // A different white goes through Bradford: ACES AP0 at D60 keeps
        // white as white, to the precision of the adaptation.
        let m = to_srgb_matrix(&chroma([
            (0.7347, 0.2653),
            (0.0, 1.0),
            (0.0001, -0.0770),
            (0.32168, 0.33767),
        ]))
        .unwrap();
        let white = mul_vec(m, [1.0, 1.0, 1.0]);
        assert!(white.iter().all(|c| (c - 1.0).abs() < 2e-3), "{white:?}");
        // Degenerate chromaticities do not panic.
        assert!(
            to_srgb_matrix(&chroma([(0.0, 0.0), (0.0, 0.0), (0.0, 0.0), (0.0, 0.0)])).is_none()
        );
    }

    /// An EXR written by `exr` itself with Display P3 chromaticities: a
    /// pure P3 red decodes to more red than sRGB can show and negative
    /// green, a neutral stays neutral.
    #[test]
    fn a_p3_tagged_file_is_rotated_to_srgb_primaries() {
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
        let v = img.samples().as_f32().unwrap();
        assert!(v[0] > 1.2 && v[1] < -0.03 && v[2].abs() < 0.03, "{v:?}");
        assert!(v[3..6].iter().all(|s| (s - 0.5).abs() < 1e-3), "{v:?}");
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
