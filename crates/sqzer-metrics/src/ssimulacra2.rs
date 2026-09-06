//! SSIMULACRA2 over `fast-ssim2`.
//!
//! Sample handling follows the metric's conventions: `u8` and `u16`
//! samples are taken as sRGB and linearised, `f32` samples are taken as
//! linear light and passed through, gray is replicated into three
//! channels and alpha is ignored. An ICC profile on the image is not
//! consulted; converting to sRGB before scoring is the pipeline's job.
//!
//! Images below 8 pixels on a side are reflect-padded by `fast-ssim2`
//! before scoring, so even a 1x1 image gets a number. Treat scores on such
//! images as an ordering, not a calibrated quality.

use fast_ssim2::{
    CompareContext, LinearRgbImage, Ssimulacra2Error, Ssimulacra2Reference, compute_ssimulacra2,
    srgb_u8_to_linear, srgb_u16_to_linear,
};
use sqzer_core::image::{Image, Samples};
use sqzer_core::metric::Metric;
use sqzer_core::{Error, Result};

/// The SSIMULACRA2 metric. 100 is identical, 90 and above is
/// imperceptible, 70 is high quality on a normal display, 50 is medium,
/// negative is possible.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ssimulacra2;

impl Metric for Ssimulacra2 {
    fn name(&self) -> &'static str {
        "ssimulacra2"
    }

    fn score(&self, reference: &Image, distorted: &Image) -> Result<f32> {
        check_dimensions(reference, distorted)?;
        compute_ssimulacra2(linear(reference), linear(distorted))
            .map(narrow)
            .map_err(convert)
    }
}

/// A reference image with the metric's reference-side work done once.
/// Scoring a candidate against it is about twice as fast as
/// [`Ssimulacra2::score`], which matters when a search scores several
/// encodes of the same source.
///
/// Holds working buffers, so it is `Send` but not `Sync`: one per thread.
pub struct Reference {
    inner: Ssimulacra2Reference,
    ctx: CompareContext,
    width: u32,
    height: u32,
}

impl Reference {
    /// Precompute the reference side of the metric for `reference`.
    ///
    /// # Errors
    /// [`Error::TooLarge`] above the metric's own pixel limit.
    pub fn new(reference: &Image) -> Result<Self> {
        let inner = Ssimulacra2Reference::new(linear(reference)).map_err(convert)?;
        let ctx = inner.compare_context();
        Ok(Self {
            inner,
            ctx,
            width: reference.width(),
            height: reference.height(),
        })
    }

    /// Score `distorted` against the reference.
    ///
    /// # Errors
    /// [`Error::InvalidInput`] on a dimension mismatch.
    pub fn score(&mut self, distorted: &Image) -> Result<f32> {
        if (distorted.width(), distorted.height()) != (self.width, self.height) {
            return Err(Error::InvalidInput(format!(
                "metric inputs differ in size: reference {}x{}, candidate {}x{}",
                self.width,
                self.height,
                distorted.width(),
                distorted.height()
            )));
        }
        self.inner
            .compare_with(&mut self.ctx, linear(distorted))
            .map(narrow)
            .map_err(convert)
    }

    /// Width of the reference.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height of the reference.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }
}

impl core::fmt::Debug for Reference {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Reference")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

fn check_dimensions(a: &Image, b: &Image) -> Result<()> {
    if (a.width(), a.height()) != (b.width(), b.height()) {
        return Err(Error::InvalidInput(format!(
            "metric inputs differ in size: reference {}x{}, candidate {}x{}",
            a.width(),
            a.height(),
            b.width(),
            b.height()
        )));
    }
    Ok(())
}

/// The metric works in `f64`; the trait reports `f32`. Scores live in
/// roughly -100..=100 with a handful of meaningful decimals, so nothing
/// is lost.
#[allow(clippy::cast_possible_truncation)]
fn narrow(score: f64) -> f32 {
    score as f32
}

fn convert(e: Ssimulacra2Error) -> Error {
    match e {
        Ssimulacra2Error::ImageTooLarge { actual } => Error::TooLarge {
            pixels: actual as u64,
            limit: fast_ssim2::MAX_IMAGE_PIXELS as u64,
        },
        Ssimulacra2Error::NonMatchingImageDimensions | Ssimulacra2Error::InvalidImageSize => {
            Error::InvalidInput(e.to_string())
        }
        Ssimulacra2Error::LinearRgbConversionFailed | Ssimulacra2Error::GaussianBlurError => {
            Error::Codec(format!("ssimulacra2: {e}"))
        }
    }
}

/// `Image` to the metric's linear RGB buffer. Alpha is dropped, gray is
/// replicated.
fn linear(img: &Image) -> LinearRgbImage {
    let ch = img.channels();
    let gray = ch < 3;
    let data: Vec<[f32; 3]> = match img.samples() {
        Samples::U8(v) => v
            .chunks_exact(ch)
            .map(|px| rgb(px, gray, srgb_u8_to_linear))
            .collect(),
        Samples::U16(v) => v
            .chunks_exact(ch)
            .map(|px| rgb(px, gray, srgb_u16_to_linear))
            .collect(),
        Samples::F32(v) => v.chunks_exact(ch).map(|px| rgb(px, gray, |s| s)).collect(),
    };
    LinearRgbImage::new(data, img.width() as usize, img.height() as usize)
}

#[inline]
fn rgb<T: Copy>(px: &[T], gray: bool, f: impl Fn(T) -> f32) -> [f32; 3] {
    if gray {
        let l = f(px[0]);
        [l, l, l]
    } else {
        [f(px[0]), f(px[1]), f(px[2])]
    }
}

#[cfg(test)]
// Synthetic pixel data: the truncating casts are the point.
#[allow(clippy::cast_possible_truncation, clippy::many_single_char_names)]
mod tests {
    use super::*;
    use sqzer_core::image::ColorType;

    fn gradient(color: ColorType, w: u32, h: u32) -> Image {
        let ch = color.channels();
        let mut v = Vec::with_capacity((w * h) as usize * ch);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / (w - 1).max(1)) as u8;
                let g = (y * 255 / (h - 1).max(1)) as u8;
                let b = if x < w / 2 { 40 } else { 220 };
                let gray = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
                match color {
                    ColorType::Gray => v.push(gray),
                    ColorType::GrayAlpha => v.extend([gray, 128]),
                    ColorType::Rgb => v.extend([r, g, b]),
                    ColorType::Rgba => v.extend([r, g, b, 128]),
                }
            }
        }
        Image::from_u8(w, h, color, v).unwrap()
    }

    #[test]
    fn identical_images_score_100() {
        let img = gradient(ColorType::Rgb, 32, 24);
        let s = Ssimulacra2.score(&img, &img).unwrap();
        assert!((s - 100.0).abs() < 1e-3, "{s}");
    }

    #[test]
    fn alpha_and_gray_are_scored_as_colour() {
        let rgb = gradient(ColorType::Rgb, 32, 24);
        let rgba = gradient(ColorType::Rgba, 32, 24);
        assert!((Ssimulacra2.score(&rgb, &rgba).unwrap() - 100.0).abs() < 1e-3);

        let gray = gradient(ColorType::Gray, 32, 24);
        let ga = gradient(ColorType::GrayAlpha, 32, 24);
        assert!((Ssimulacra2.score(&gray, &ga).unwrap() - 100.0).abs() < 1e-3);
    }

    #[test]
    fn u16_by_replication_matches_u8() {
        let img = gradient(ColorType::Rgb, 32, 24);
        let wide: Vec<u16> = img
            .samples()
            .as_u8()
            .unwrap()
            .iter()
            .map(|&s| u16::from(s) * 257)
            .collect();
        let wide = Image::from_u16(32, 24, ColorType::Rgb, wide).unwrap();
        let s = Ssimulacra2.score(&img, &wide).unwrap();
        assert!(s > 99.9, "{s}");
    }

    #[test]
    fn distortion_lowers_the_score_monotonically() {
        let img = gradient(ColorType::Rgb, 48, 32);
        let mut prev = 100.0f32;
        for step in [4u8, 16, 64] {
            let coarse: Vec<u8> = img
                .samples()
                .as_u8()
                .unwrap()
                .iter()
                .map(|&s| s / step * step)
                .collect();
            let coarse = Image::from_u8(48, 32, ColorType::Rgb, coarse).unwrap();
            let s = Ssimulacra2.score(&img, &coarse).unwrap();
            assert!(s < prev, "step {step}: {s} not below {prev}");
            prev = s;
        }
    }

    #[test]
    fn reference_agrees_with_one_shot() {
        let img = gradient(ColorType::Rgb, 48, 32);
        let coarse: Vec<u8> = img
            .samples()
            .as_u8()
            .unwrap()
            .iter()
            .map(|&s| s / 16 * 16)
            .collect();
        let coarse = Image::from_u8(48, 32, ColorType::Rgb, coarse).unwrap();
        let one_shot = Ssimulacra2.score(&img, &coarse).unwrap();
        let mut reference = Reference::new(&img).unwrap();
        let a = reference.score(&coarse).unwrap();
        let b = reference.score(&coarse).unwrap();
        assert!((a - one_shot).abs() < 1e-3, "{a} vs {one_shot}");
        assert!((a - b).abs() < 1e-6, "repeat");
    }

    #[test]
    fn size_mismatch_is_invalid_input() {
        let a = gradient(ColorType::Rgb, 32, 24);
        let b = gradient(ColorType::Rgb, 24, 32);
        assert!(matches!(
            Ssimulacra2.score(&a, &b),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            Reference::new(&a).unwrap().score(&b),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn tiny_images_are_scored_not_refused() {
        for (w, h) in [(1, 1), (3, 2), (7, 7), (8, 1)] {
            let img = gradient(ColorType::Rgb, w, h);
            let s = Ssimulacra2.score(&img, &img).unwrap();
            assert!(s.is_finite(), "{w}x{h}: {s}");
        }
    }
}
