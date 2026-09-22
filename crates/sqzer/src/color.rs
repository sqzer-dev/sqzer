//! The colour stage of ADR-0007, an adapter over `moxcms`.
//!
//! An image that carries an ICC profile is converted to sRGB and the
//! profile dropped, so the encoder gets untagged sRGB and the resize and
//! the metric, which both assume sRGB, are exact. Which profiles convert:
//!
//! ```text
//! RGB profile on Rgb / Rgba          converted, alpha carried through
//! Gray profile on Gray / GrayAlpha   converted against an sRGB-curve gray profile
//! any other pairing                  no defined conversion: profile dropped, samples read as sRGB
//! unparseable profile                `Error::Transform`
//! ```
//!
//! `u8` and `u16` samples convert in their own depth. `f32` samples are
//! linear light by the [`Image`] contract, so only the primaries of a
//! matrix-shaper profile apply, as one linear matrix; a LUT profile on
//! float samples is refused.

use moxcms::{ColorProfile, DataColorSpace, Layout, TransformExecutor, TransformOptions};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::{Error, Result};

/// `image` converted to sRGB with its profile removed, or `image` itself
/// when it carries none.
pub fn to_srgb(image: Image) -> Result<Image> {
    let Some(icc) = image.icc() else {
        return Ok(image);
    };
    let profile = ColorProfile::new_from_slice(icc).map_err(failed)?;
    let (width, height, color, samples, _) = image.into_parts();
    let layout = layout(color);
    let target = match (profile.color_space, color) {
        (DataColorSpace::Rgb, ColorType::Rgb | ColorType::Rgba) => ColorProfile::new_srgb(),
        (DataColorSpace::Gray, ColorType::Gray | ColorType::GrayAlpha) => gray_srgb(),
        _ => return Image::new(width, height, color, samples),
    };
    let options = TransformOptions {
        rendering_intent: profile.rendering_intent,
        ..TransformOptions::default()
    };
    let samples = match samples {
        Samples::U8(v) => {
            let t = profile
                .create_transform_8bit(layout, &target, layout, options)
                .map_err(failed)?;
            Samples::U8(run(&*t, &v)?)
        }
        Samples::U16(v) => {
            let t = profile
                .create_transform_16bit(layout, &target, layout, options)
                .map_err(failed)?;
            Samples::U16(run(&*t, &v)?)
        }
        Samples::F32(v) => Samples::F32(linear_matrix(&profile, &target, color, v)?),
    };
    Image::new(width, height, color, samples)
}

fn run<T: Copy + Default>(t: &dyn TransformExecutor<T>, src: &[T]) -> Result<Vec<T>> {
    let mut out = vec![T::default(); src.len()];
    t.transform(src, &mut out).map_err(failed)?;
    Ok(out)
}

/// Linear samples in the profile's primaries to linear sRGB primaries.
/// Gray has one primary and nothing to rotate, so it passes through.
fn linear_matrix(
    profile: &ColorProfile,
    target: &ColorProfile,
    color: ColorType,
    mut v: Vec<f32>,
) -> Result<Vec<f32>> {
    if !profile.is_matrix_shaper() {
        return Err(failed(
            "float samples are linear light; only a matrix-shaper profile can describe them",
        ));
    }
    if matches!(color, ColorType::Gray | ColorType::GrayAlpha) {
        return Ok(v);
    }
    let matrix = profile.transform_matrix(target).v;
    for px in v.chunks_exact_mut(color.channels()) {
        let input = [f64::from(px[0]), f64::from(px[1]), f64::from(px[2])];
        for (out, row) in px.iter_mut().zip(matrix) {
            let sum: f64 = row.iter().zip(input).map(|(k, x)| k * x).sum();
            // Out of gamut below zero is clamped; above one is kept, this
            // is HDR data.
            #[allow(clippy::cast_possible_truncation)]
            {
                *out = sum.max(0.0) as f32;
            }
        }
    }
    Ok(v)
}

/// A gray profile with sRGB's tone curve: the gray side of "convert to
/// sRGB".
fn gray_srgb() -> ColorProfile {
    let srgb = ColorProfile::new_srgb();
    let mut gray = ColorProfile::new_gray_with_gamma(2.2);
    gray.gray_trc = srgb.red_trc;
    gray
}

const fn layout(color: ColorType) -> Layout {
    match color {
        ColorType::Gray => Layout::Gray,
        ColorType::GrayAlpha => Layout::GrayAlpha,
        ColorType::Rgb => Layout::Rgb,
        ColorType::Rgba => Layout::Rgba,
    }
}

fn failed(e: impl std::fmt::Display) -> Error {
    Error::Transform {
        stage: "color",
        message: e.to_string(),
    }
}

#[cfg(test)]
// Alpha samples pass through unchanged, so exact float comparison is the
// assertion.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn p3() -> Vec<u8> {
        ColorProfile::new_display_p3().encode().unwrap()
    }

    fn srgb() -> Vec<u8> {
        ColorProfile::new_srgb().encode().unwrap()
    }

    fn rgb8(px: &[[u8; 3]], icc: Vec<u8>) -> Image {
        let n = u32::try_from(px.len()).unwrap();
        Image::from_u8(n, 1, ColorType::Rgb, px.concat())
            .unwrap()
            .with_icc(Some(icc))
    }

    #[test]
    fn untagged_images_pass_through() {
        let img = Image::from_u8(1, 1, ColorType::Rgb, vec![1, 2, 3]).unwrap();
        assert_eq!(to_srgb(img.clone()).unwrap(), img);
    }

    #[test]
    fn p3_to_srgb_keeps_neutrals_and_raises_saturation() {
        let out = to_srgb(rgb8(
            &[
                [0, 0, 0],
                [128, 128, 128],
                [255, 255, 255],
                [200, 100, 100],
                [60, 180, 90],
            ],
            p3(),
        ))
        .unwrap();
        assert_eq!(out.icc(), None);
        let v = out.samples().as_u8().unwrap();
        // Same white, same curve: neutrals do not move.
        assert_eq!(&v[..3], &[0, 0, 0]);
        assert!(v[3..6].iter().all(|&s| s.abs_diff(128) <= 1), "{v:?}");
        assert_eq!(&v[6..9], &[255, 255, 255]);
        // A P3 colour is wider than the same numbers in sRGB, so in sRGB
        // it reads more saturated: the dominant channel up, the others down.
        assert!(v[9] > 200 && v[10] < 100 && v[11] < 100, "{v:?}");
        assert!(v[13] > 180 && v[12] < 60, "{v:?}");
    }

    #[test]
    fn srgb_profile_is_an_identity_within_one_step() {
        let px: Vec<[u8; 3]> = (0..=255u8).map(|s| [s, 255 - s, s / 2]).collect();
        let out = to_srgb(rgb8(&px, srgb())).unwrap();
        let v = out.samples().as_u8().unwrap();
        let worst = v
            .iter()
            .zip(px.concat())
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap();
        assert!(worst <= 1, "worst step {worst}");
    }

    #[test]
    fn alpha_and_sixteen_bit_survive() {
        let img = Image::from_u16(
            2,
            1,
            ColorType::Rgba,
            vec![51_400, 25_700, 25_700, 77, 0, 0, 0, 65_535],
        )
        .unwrap()
        .with_icc(Some(p3()));
        let out = to_srgb(img).unwrap();
        assert_eq!(out.color(), ColorType::Rgba);
        let v = out.samples().as_u16().unwrap();
        assert_eq!((v[3], v[7]), (77, 65_535));
        assert!(v[0] > 51_400 && v[1] < 25_700, "{v:?}");
        assert_eq!(&v[4..7], &[0, 0, 0]);
    }

    #[test]
    fn gray_profile_converts_the_curve() {
        let gamma = ColorProfile::new_gray_with_gamma(2.2).encode().unwrap();
        let img = Image::from_u8(3, 1, ColorType::GrayAlpha, vec![10, 200, 128, 9, 255, 3])
            .unwrap()
            .with_icc(Some(gamma));
        let out = to_srgb(img).unwrap();
        assert_eq!(out.icc(), None);
        let v = out.samples().as_u8().unwrap();
        // Gamma 2.2 is darker than sRGB's curve in the shadows and meets
        // it near the middle; alpha is not a colour.
        assert!(v[0] < 10, "{v:?}");
        assert!(v[2].abs_diff(128) <= 2, "{v:?}");
        assert_eq!((v[1], v[3], v[4], v[5]), (200, 9, 255, 3));
    }

    #[test]
    fn a_profile_for_another_layout_is_dropped() {
        let img = Image::from_u8(1, 1, ColorType::Gray, vec![77])
            .unwrap()
            .with_icc(Some(p3()));
        let out = to_srgb(img).unwrap();
        assert_eq!(out.icc(), None);
        assert_eq!(out.samples().as_u8(), Some(&[77][..]));
    }

    #[test]
    fn garbage_is_an_error_not_a_pass() {
        let img = Image::from_u8(1, 1, ColorType::Rgb, vec![1, 2, 3])
            .unwrap()
            .with_icc(Some(b"not really a profile".to_vec()));
        let err = to_srgb(img).unwrap_err();
        assert!(
            matches!(err, Error::Transform { stage: "color", .. }),
            "{err}"
        );
    }

    #[test]
    fn float_samples_take_the_primaries_only() {
        let img = Image::new(
            2,
            1,
            ColorType::Rgba,
            Samples::F32(vec![1.0, 0.0, 0.0, 0.5, 0.25, 0.25, 0.25, 1.0]),
        )
        .unwrap()
        .with_icc(Some(p3()));
        let out = to_srgb(img).unwrap();
        let v = out.samples().as_f32().unwrap();
        // P3 red is outside sRGB: more than all of sRGB's red, and a
        // negative green clamped away. Alpha and neutrals untouched.
        assert!(v[0] > 1.0 && v[1] == 0.0, "{v:?}");
        assert_eq!(v[3], 0.5);
        assert!(v[4..7].iter().all(|&s| (s - 0.25).abs() < 1e-3), "{v:?}");
        assert_eq!(v[7], 1.0);
    }
}
