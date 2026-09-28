//! The resize request of ADR-0009 and its geometry.
//!
//! A [`Resize`] says how an image meets a box, in CSS `object-fit` terms,
//! or scales it by a factor. [`Resize::fit`] turns it into a [`Geometry`]
//! for one image: the part of the source that is kept, the size it is
//! resampled to, and the canvas it is padded to. This is the geometry
//! only; the resampling and the padding are done by the pipeline in
//! `sqzer`.

use crate::{Error, Result};

/// What the image is sized against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    /// A box. A side that is `None` does not constrain; only
    /// [`Fit::Inside`] takes a box with a side missing.
    Box {
        /// Width in pixels.
        width: Option<u32>,
        /// Height in pixels.
        height: Option<u32>,
    },
    /// A factor on both axes, keeping the aspect ratio. `0.5` halves the
    /// image.
    Scale(f32),
}

impl Default for Size {
    fn default() -> Self {
        Self::Box {
            width: None,
            height: None,
        }
    }
}

/// How the image meets the box, as in CSS `object-fit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Fit {
    /// Fit inside the box, keep the aspect ratio, no padding. One side may
    /// be given alone.
    #[default]
    Inside,
    /// Cover the box, keep the aspect ratio, crop the overflow at the
    /// [`Position`].
    Cover,
    /// Fit inside the box, keep the aspect ratio, pad to the box.
    Contain,
    /// Scale each axis to the box; the aspect ratio changes.
    Fill,
}

impl Fit {
    /// The name as CSS and the CLI spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Inside => "inside",
            Self::Cover => "cover",
            Self::Contain => "contain",
            Self::Fill => "fill",
        }
    }
}

/// Where [`Fit::Cover`] crops and [`Fit::Contain`] places the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Position {
    /// Centred on both axes.
    #[default]
    Center,
    /// Top edge, centred horizontally.
    Top,
    /// Bottom edge, centred horizontally.
    Bottom,
    /// Left edge, centred vertically.
    Left,
    /// Right edge, centred vertically.
    Right,
    /// Top-left corner.
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
}

impl Position {
    /// The point as fractions of the free space, `(0.0, 0.0)` top-left to
    /// `(1.0, 1.0)` bottom-right. A cover crop keeps that share of the
    /// overflow on the left and top; a contain pad puts that share of the
    /// padding there.
    #[must_use]
    pub const fn anchor(self) -> (f64, f64) {
        match self {
            Self::Center => (0.5, 0.5),
            Self::Top => (0.5, 0.0),
            Self::Bottom => (0.5, 1.0),
            Self::Left => (0.0, 0.5),
            Self::Right => (1.0, 0.5),
            Self::TopLeft => (0.0, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::BottomLeft => (0.0, 1.0),
            Self::BottomRight => (1.0, 1.0),
        }
    }
}

/// The resampling filter. The pipeline maps each to `fast_image_resize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Filter {
    /// Lanczos3, sharp with little ringing. The default.
    #[default]
    Lanczos3,
    /// Mitchell-Netravali bicubic, softer than Lanczos3.
    Mitchell,
    /// Catmull-Rom bicubic.
    CatmullRom,
    /// Bilinear.
    Bilinear,
    /// Box: each source pixel contributes equally.
    Box,
    /// Nearest neighbour, for pixel art: no new colours.
    Nearest,
}

/// A resize request, ADR-0009 D4.
///
/// [`Resize::NONE`], the default, leaves every image alone. The
/// `thumbnail` preset is [`Resize::inside`] 512 x 512.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Resize {
    /// The box or the factor.
    pub size: Size,
    /// How the image meets the box. Ignored for [`Size::Scale`].
    pub fit: Fit,
    /// Where [`Fit::Cover`] crops and [`Fit::Contain`] places the image.
    pub position: Position,
    /// The padding of [`Fit::Contain`], RGBA with sRGB-encoded colour.
    /// `None` is the per-output default: transparent for an encoder that
    /// takes alpha, white for one that does not.
    pub background: Option<[u8; 4]>,
    /// Allow scaling up. Without it no fit ever scales up (ADR-0009 D2).
    pub enlarge: bool,
    /// The resampling filter.
    pub filter: Filter,
}

/// The part of the source a [`Geometry`] keeps, in source pixels. The
/// edges may fall between pixels when the crop is scaled down after.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    /// Left edge.
    pub left: f64,
    /// Top edge.
    pub top: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// The box [`Fit::Contain`] pads the resampled image to, and where the
/// image sits in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Canvas {
    /// Canvas width.
    pub width: u32,
    /// Canvas height.
    pub height: u32,
    /// Left edge of the image on the canvas.
    pub x: u32,
    /// Top edge of the image on the canvas.
    pub y: u32,
    /// [`Resize::background`], still to be resolved against the encoder.
    pub background: Option<[u8; 4]>,
}

/// What [`Resize::fit`] does to one image: crop, resample, pad, in that
/// order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// The part of the source kept, or `None` for all of it.
    pub crop: Option<Crop>,
    /// Width the kept part is resampled to.
    pub width: u32,
    /// Height the kept part is resampled to.
    pub height: u32,
    /// The canvas the result is padded to, or `None` for no padding.
    pub canvas: Option<Canvas>,
}

impl Geometry {
    /// The size the encoder gets: the canvas when there is one.
    #[must_use]
    pub fn output(&self) -> (u32, u32) {
        self.canvas
            .map_or((self.width, self.height), |c| (c.width, c.height))
    }

    /// Whether a `width` x `height` source needs the resampler, or only
    /// the padding.
    #[must_use]
    pub fn resamples(&self, width: u32, height: u32) -> bool {
        self.crop.is_some() || (self.width, self.height) != (width, height)
    }
}

impl Resize {
    /// No resize: every image passes through untouched.
    pub const NONE: Self = Self {
        size: Size::Box {
            width: None,
            height: None,
        },
        fit: Fit::Inside,
        position: Position::Center,
        background: None,
        enlarge: false,
        filter: Filter::Lanczos3,
    };

    /// Fit inside `width` x `height`, keep the aspect ratio, never enlarge:
    /// `--max-width` and `--max-height`.
    #[must_use]
    pub const fn inside(width: Option<u32>, height: Option<u32>) -> Self {
        Self {
            size: Size::Box { width, height },
            ..Self::NONE
        }
    }

    /// Refuse a request that has no geometry: a zero side, a fit other
    /// than [`Fit::Inside`] without both sides, or a factor that is not a
    /// positive finite number.
    ///
    /// # Errors
    /// [`Error::InvalidParams`] naming the problem.
    pub fn check(&self) -> Result<()> {
        match self.size {
            Size::Scale(f) if !(f.is_finite() && f > 0.0) => Err(Error::InvalidParams(format!(
                "a resize factor must be a positive number, got {f}"
            ))),
            Size::Scale(_) => Ok(()),
            Size::Box { width, height } => {
                if width == Some(0) || height == Some(0) {
                    return Err(Error::InvalidParams(
                        "a resize bound must be at least one pixel".into(),
                    ));
                }
                if self.fit != Fit::Inside && (width.is_none() || height.is_none()) {
                    return Err(Error::InvalidParams(format!(
                        "fit {} needs both a width and a height",
                        self.fit.name()
                    )));
                }
                Ok(())
            }
        }
    }

    /// The geometry for a `width` x `height` image, or `None` when the
    /// image is left as it is. Sizes are rounded to the nearest pixel and
    /// never drop below one. A request [`Resize::check`] refuses is read
    /// leniently: a zero side as one, a missing side of a box as no
    /// constraint under [`Fit::Inside`], a bad factor as no resize.
    #[must_use]
    pub fn fit(&self, width: u32, height: u32) -> Option<Geometry> {
        let (w, h) = (u64::from(width.max(1)), u64::from(height.max(1)));
        let anchor = self.position.anchor();
        let (bw, bh) = match self.size {
            Size::Scale(f) => return self.scale(w, h, f),
            Size::Box { width, height } => (
                width.map(|v| u64::from(v.max(1))),
                height.map(|v| u64::from(v.max(1))),
            ),
        };
        let geometry = match (self.fit, bw, bh) {
            (Fit::Cover, Some(bw), Some(bh)) => cover(w, h, bw, bh, self.enlarge, anchor),
            (Fit::Contain, Some(bw), Some(bh)) => {
                let (iw, ih) = inside(w, h, Some(bw), Some(bh), self.enlarge);
                let canvas = ((iw, ih) != (bw, bh)).then(|| Canvas {
                    width: clamp(bw),
                    height: clamp(bh),
                    x: offset(bw - iw, anchor.0),
                    y: offset(bh - ih, anchor.1),
                    background: self.background,
                });
                Geometry {
                    crop: None,
                    width: clamp(iw),
                    height: clamp(ih),
                    canvas,
                }
            }
            (Fit::Fill, Some(bw), Some(bh)) => {
                let (fw, fh) = if self.enlarge {
                    (bw, bh)
                } else {
                    (bw.min(w), bh.min(h))
                };
                plain(fw, fh)
            }
            _ => {
                let (iw, ih) = inside(w, h, bw, bh, self.enlarge);
                plain(iw, ih)
            }
        };
        (geometry.crop.is_some()
            || geometry.canvas.is_some()
            || (geometry.width, geometry.height) != (width, height))
            .then_some(geometry)
    }

    // The float arithmetic stays well inside `f64`'s exact integer range:
    // both factors are at most `u32::MAX`.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn scale(&self, w: u64, h: u64, f: f32) -> Option<Geometry> {
        if !(f.is_finite() && f > 0.0) {
            return None;
        }
        let f = if self.enlarge {
            f64::from(f)
        } else {
            f64::from(f).min(1.0)
        };
        let side = |n: u64| ((n as f64 * f).round().max(1.0)).min(u64::MAX as f64) as u64;
        let g = plain(side(w), side(h));
        ((u64::from(g.width), u64::from(g.height)) != (w, h)).then_some(g)
    }
}

/// Resample only.
fn plain(w: u64, h: u64) -> Geometry {
    Geometry {
        crop: None,
        width: clamp(w),
        height: clamp(h),
        canvas: None,
    }
}

/// `n * num / den`, rounded to nearest, at least one. `n` and `num` are
/// at most `u32::MAX`, so the product fits.
fn ratio(n: u64, num: u64, den: u64) -> u64 {
    ((n * num + den / 2) / den).max(1)
}

/// A pixel count that may exceed `u32` after an enlarge. The caller's
/// pixel limit refuses such a size; saturating keeps it refusable.
fn clamp(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// The share `at` of `free` pixels, rounded down so the image stays
/// inside.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn offset(free: u64, at: f64) -> u32 {
    clamp((free as f64 * at).floor() as u64)
}

/// Fit inside the box: the tighter bound decides the scale, the other
/// axis is rounded. Without `enlarge`, a tighter bound at or above its
/// own side leaves the image as it is.
fn inside(w: u64, h: u64, bw: Option<u64>, bh: Option<u64>, enlarge: bool) -> (u64, u64) {
    // `bw / w <= bh / h`, cross-multiplied: the width bound is tighter.
    let by_width = match (bw, bh) {
        (Some(bw), Some(bh)) => bw * h <= bh * w,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => return (w, h),
    };
    if by_width {
        let bw = bw.unwrap_or(w);
        if !enlarge && bw >= w {
            return (w, h);
        }
        (bw, ratio(h, bw, w))
    } else {
        let bh = bh.unwrap_or(h);
        if !enlarge && bh >= h {
            return (w, h);
        }
        (ratio(w, bh, h), bh)
    }
}

/// Cover the box. The kept part is the largest rectangle of the box's
/// aspect ratio inside the source, placed at `anchor`. When that part is
/// smaller than the box and enlarging is off, it is kept at its own
/// resolution, rounded to whole pixels: the output has the box's aspect
/// ratio and is smaller than the box.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn cover(w: u64, h: u64, bw: u64, bh: u64, enlarge: bool, anchor: (f64, f64)) -> Geometry {
    let (wf, hf) = (w as f64, h as f64);
    // `bw / bh >= w / h`: the box is wider, so the width is kept whole.
    let (cw, ch) = if bw * h >= bh * w {
        (wf, wf * bh as f64 / bw as f64)
    } else {
        (hf * bw as f64 / bh as f64, hf)
    };
    let (out_w, out_h, cw, ch) = if enlarge || cw >= bw as f64 {
        (bw, bh, cw, ch)
    } else {
        let rw = (cw.round() as u64).clamp(1, w);
        let rh = (ch.round() as u64).clamp(1, h);
        (rw, rh, rw as f64, rh as f64)
    };
    let crop = (cw < wf || ch < hf).then(|| Crop {
        left: ((wf - cw) * anchor.0).floor(),
        top: ((hf - ch) * anchor.1).floor(),
        width: cw,
        height: ch,
    });
    Geometry {
        crop,
        width: clamp(out_w),
        height: clamp(out_h),
        canvas: None,
    }
}

#[cfg(test)]
// The crop edges compared are whole numbers or one exact quotient.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn boxed(width: Option<u32>, height: Option<u32>, fit: Fit) -> Resize {
        Resize {
            size: Size::Box { width, height },
            fit,
            ..Resize::NONE
        }
    }

    fn sized(r: Resize, w: u32, h: u32) -> Option<(u32, u32)> {
        r.fit(w, h).map(|g| g.output())
    }

    #[test]
    fn inside_keeps_the_aspect_ratio_inside_both_bounds() {
        let both = |w, h| Resize::inside(Some(w), Some(h));
        let width = |w| Resize::inside(Some(w), None);
        let height = |h| Resize::inside(None, Some(h));
        assert_eq!(sized(width(1600), 4000, 3000), Some((1600, 1200)));
        assert_eq!(sized(height(600), 4000, 3000), Some((800, 600)));
        // The tighter bound decides, whichever axis it is on.
        assert_eq!(sized(both(1600, 600), 4000, 3000), Some((800, 600)));
        assert_eq!(sized(both(400, 3000), 4000, 3000), Some((400, 300)));
        assert_eq!(sized(both(512, 512), 3000, 4000), Some((384, 512)));
        // Rounded to nearest, not truncated.
        assert_eq!(sized(width(100), 300, 200), Some((100, 67)));
        assert_eq!(sized(width(2), 3, 1), Some((2, 1)));
        // A sliver keeps one pixel.
        assert_eq!(sized(width(10), 10_000, 3), Some((10, 1)));
        assert_eq!(sized(height(10), 3, 10_000), Some((1, 10)));
        assert_eq!(sized(width(0), 8, 8), Some((1, 1)));
        // Large inputs do not overflow.
        assert_eq!(
            sized(both(65_535, 65_535), u32::MAX, u32::MAX),
            Some((65_535, 65_535))
        );
        // Inside only resamples.
        let g = width(1600).fit(4000, 3000).unwrap();
        assert_eq!((g.crop, g.canvas), (None, None));
    }

    #[test]
    fn inside_never_enlarges_unless_asked() {
        let r = Resize::inside(Some(1600), Some(1600));
        assert_eq!(r.fit(1600, 1200), None);
        assert_eq!(r.fit(640, 480), None);
        assert_eq!(r.fit(1, 1), None);
        assert_eq!(Resize::NONE.fit(4000, 3000), None);
        // One axis inside its bound, the other not: still a downscale.
        assert_eq!(sized(r, 3200, 100), Some((1600, 50)));

        let up = Resize { enlarge: true, ..r };
        assert_eq!(sized(up, 640, 480), Some((1600, 1200)));
        assert_eq!(sized(up, 1, 1), Some((1600, 1600)));
        // A box the image already fills exactly is still left alone.
        assert_eq!(up.fit(1600, 900), None);
        // One side alone enlarges on that side, the other follows.
        let wide = Resize {
            enlarge: true,
            ..Resize::inside(Some(1000), None)
        };
        assert_eq!(sized(wide, 100, 300), Some((1000, 3000)));
        // An enlarge past `u32` saturates, for the pixel limit to refuse.
        let huge = Resize {
            enlarge: true,
            ..Resize::inside(Some(u32::MAX), None)
        };
        assert_eq!(sized(huge, 1, 2), Some((u32::MAX, u32::MAX)));
    }

    #[test]
    fn cover_fills_the_box_and_crops_at_the_position() {
        let r = boxed(Some(400), Some(400), Fit::Cover);
        // Landscape into a square: the full height is kept, the sides go.
        let g = r.fit(800, 600).unwrap();
        assert_eq!(g.output(), (400, 400));
        let crop = g.crop.unwrap();
        assert_eq!((crop.width, crop.height), (600.0, 600.0));
        assert_eq!((crop.left, crop.top), (100.0, 0.0));
        // Positions move the crop along the overflowing axis only.
        let at = |p| {
            let c = Resize { position: p, ..r }
                .fit(800, 600)
                .unwrap()
                .crop
                .unwrap();
            (c.left, c.top)
        };
        assert_eq!(at(Position::Left), (0.0, 0.0));
        assert_eq!(at(Position::Right), (200.0, 0.0));
        assert_eq!(at(Position::TopRight), (200.0, 0.0));
        assert_eq!(at(Position::Top), (100.0, 0.0));
        // Portrait into a landscape box: the full width is kept.
        let g = boxed(Some(300), Some(100), Fit::Cover)
            .fit(600, 900)
            .unwrap();
        let crop = g.crop.unwrap();
        assert_eq!((crop.width, crop.height), (600.0, 200.0));
        assert_eq!(crop.top, 350.0);
        assert_eq!(g.output(), (300, 100));
        // The same aspect ratio needs no crop, only a resample.
        let g = r.fit(800, 800).unwrap();
        assert_eq!((g.crop, g.output()), (None, (400, 400)));
        assert_eq!(r.fit(400, 400), None);
    }

    #[test]
    fn cover_without_enlarge_crops_at_the_source_resolution() {
        let r = boxed(Some(1000), Some(500), Fit::Cover);
        // 600 x 600 cannot cover 1000 x 500 without scaling up: keep the
        // 2:1 part at 600 x 300, pixel for pixel.
        let g = r.fit(600, 600).unwrap();
        assert_eq!(g.output(), (600, 300));
        let crop = g.crop.unwrap();
        assert_eq!(
            (crop.left, crop.top, crop.width, crop.height),
            (0.0, 150.0, 600.0, 300.0)
        );
        // A source of the box's shape but smaller is left alone.
        assert_eq!(r.fit(500, 250), None);
        // With enlarge it covers the box.
        let up = Resize { enlarge: true, ..r };
        let g = up.fit(600, 600).unwrap();
        assert_eq!(g.output(), (1000, 500));
        assert_eq!(g.crop.unwrap().height, 300.0);
    }

    #[test]
    fn contain_pads_to_the_box_and_places_at_the_position() {
        let r = boxed(Some(400), Some(400), Fit::Contain);
        let g = r.fit(800, 600).unwrap();
        assert_eq!((g.width, g.height), (400, 300));
        assert_eq!(g.crop, None);
        let c = g.canvas.unwrap();
        assert_eq!((c.width, c.height, c.x, c.y), (400, 400, 0, 50));
        assert_eq!(g.output(), (400, 400));
        let bottom = Resize {
            position: Position::Bottom,
            ..r
        };
        assert_eq!(bottom.fit(800, 600).unwrap().canvas.unwrap().y, 100);
        // A small image is padded to the full box without scaling up.
        let g = r.fit(100, 50).unwrap();
        assert_eq!((g.width, g.height), (100, 50));
        assert!(!g.resamples(100, 50));
        let c = g.canvas.unwrap();
        assert_eq!((c.width, c.height, c.x, c.y), (400, 400, 150, 175));
        // An image the box's shape needs no canvas.
        let g = r.fit(800, 800).unwrap();
        assert_eq!((g.canvas, g.output()), (None, (400, 400)));
        assert_eq!(r.fit(400, 400), None);
        // The background travels with the canvas.
        let white = Resize {
            background: Some([255; 4]),
            ..r
        };
        assert_eq!(
            white.fit(8, 4).unwrap().canvas.unwrap().background,
            Some([255; 4])
        );
    }

    #[test]
    fn fill_scales_each_axis_on_its_own() {
        let r = boxed(Some(400), Some(400), Fit::Fill);
        assert_eq!(sized(r, 800, 600), Some((400, 400)));
        // Without enlarge each axis is capped on its own.
        assert_eq!(sized(r, 800, 200), Some((400, 200)));
        assert_eq!(r.fit(300, 200), None);
        let up = Resize { enlarge: true, ..r };
        assert_eq!(sized(up, 300, 200), Some((400, 400)));
    }

    #[test]
    fn scale_keeps_the_aspect_ratio() {
        let half = Resize {
            size: Size::Scale(0.5),
            ..Resize::NONE
        };
        assert_eq!(sized(half, 4000, 3000), Some((2000, 1500)));
        assert_eq!(sized(half, 3, 3), Some((2, 2)));
        assert_eq!(sized(half, 1, 1), None);
        let double = Resize {
            size: Size::Scale(2.0),
            ..half
        };
        assert_eq!(double.fit(40, 30), None);
        let double = Resize {
            enlarge: true,
            ..double
        };
        assert_eq!(sized(double, 40, 30), Some((80, 60)));
        // Tiny factors keep one pixel; a bad one is no resize.
        let tiny = Resize {
            size: Size::Scale(1e-9),
            ..half
        };
        assert_eq!(sized(tiny, 4000, 3000), Some((1, 1)));
        let bad = Resize {
            size: Size::Scale(f32::NAN),
            ..half
        };
        assert_eq!(bad.fit(40, 30), None);
    }

    #[test]
    fn extreme_aspect_ratios_keep_a_pixel() {
        let cover = boxed(Some(10), Some(10), Fit::Cover);
        let g = cover.fit(100_000, 1).unwrap();
        assert_eq!(g.output(), (1, 1));
        let crop = g.crop.unwrap();
        assert_eq!((crop.width, crop.height), (1.0, 1.0));
        assert_eq!(crop.left, 49_999.0);
        let contain = boxed(Some(10), Some(10), Fit::Contain);
        let g = contain.fit(100_000, 1).unwrap();
        assert_eq!((g.width, g.height), (10, 1));
        assert_eq!(g.canvas.unwrap().y, 4);
        let tall = Resize {
            enlarge: true,
            ..boxed(Some(1), Some(100_000), Fit::Cover)
        };
        let g = tall.fit(50, 50).unwrap();
        assert_eq!(g.output(), (1, 100_000));
        assert_eq!(g.crop.unwrap().width, 50.0 / 100_000.0);
    }

    #[test]
    fn check_refuses_requests_without_a_geometry() {
        assert!(Resize::NONE.check().is_ok());
        assert!(Resize::inside(Some(10), None).check().is_ok());
        assert!(Resize::inside(Some(0), None).check().is_err());
        for fit in [Fit::Cover, Fit::Contain, Fit::Fill] {
            assert!(boxed(Some(10), Some(10), fit).check().is_ok());
            let err = boxed(Some(10), None, fit).check().unwrap_err();
            assert!(err.to_string().contains("both"), "{err}");
            // Read leniently all the same.
            assert!(boxed(Some(10), None, fit).fit(40, 40).is_some());
        }
        for f in [0.0, -1.0, f32::INFINITY, f32::NAN] {
            let r = Resize {
                size: Size::Scale(f),
                ..Resize::NONE
            };
            assert!(r.check().is_err(), "{f}");
        }
    }
}
