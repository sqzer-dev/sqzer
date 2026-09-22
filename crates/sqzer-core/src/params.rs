//! Codec-agnostic parameters. Each backend maps these to its own scale.

use std::collections::BTreeMap;

use crate::{Error, Result};

/// How to decide encoder settings.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Search encoder quality until the SSIMULACRA2 score reaches this value.
    Ssimulacra2(f32),
    /// Use this abstract quality directly, 0..=100. Disables the search.
    Quality(f32),
    /// Lossless output.
    Lossless,
}

/// A [`Target`] an encoder can act on directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Resolved {
    /// Abstract quality, already clamped to 0..=100.
    Quality(f32),
    /// Lossless output.
    Lossless,
}

/// Chroma subsampling for codecs that have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Subsampling {
    /// Let the backend choose from the quality level.
    #[default]
    Auto,
    /// No chroma subsampling.
    S444,
    /// Horizontal subsampling.
    S422,
    /// Horizontal and vertical subsampling.
    S420,
}

/// A named bundle of settings, ADR-0001 D4. A preset is a target plus an
/// effort, and for `thumbnail` a resize; codec-specific knobs stay in
/// `codec_specific`.
///
/// The targets are calibrated guesses on the SSIMULACRA2 scale, where 70
/// is "high quality, no visible artefacts on a normal display" and 50 is
/// "artefacts visible on close inspection". Revisit once real feedback
/// arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Preset {
    /// Target 70, effort 6. The default.
    #[default]
    Web,
    /// Target 60, effort 6, fit inside 512 x 512. Images that are
    /// displayed small.
    Thumbnail,
    /// Target 85, effort 8. Keep more than the eye needs, spend the time.
    Archive,
    /// Lossless, effort 8.
    Lossless,
}

impl Preset {
    /// Every preset, in a stable order.
    pub const ALL: &'static [Self] = &[Self::Web, Self::Thumbnail, Self::Archive, Self::Lossless];

    /// The preset's name as the CLI spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Thumbnail => "thumbnail",
            Self::Archive => "archive",
            Self::Lossless => "lossless",
        }
    }

    /// Parse a preset name, case-insensitively.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|p| p.name().eq_ignore_ascii_case(name))
    }

    /// The encode parameters this preset stands for.
    #[must_use]
    pub fn params(self) -> EncodeParams {
        let (target, effort) = match self {
            Self::Web => (Target::Ssimulacra2(70.0), 6),
            Self::Thumbnail => (Target::Ssimulacra2(60.0), 6),
            Self::Archive => (Target::Ssimulacra2(85.0), 8),
            Self::Lossless => (Target::Lossless, 8),
        };
        EncodeParams {
            target,
            effort,
            ..EncodeParams::default()
        }
    }

    /// The resize this preset stands for. Only `thumbnail` has one: fit
    /// inside [`Preset::THUMBNAIL_EDGE`] pixels on both axes.
    #[must_use]
    pub const fn resize(self) -> Resize {
        match self {
            Self::Thumbnail => Resize {
                max_width: Some(Self::THUMBNAIL_EDGE),
                max_height: Some(Self::THUMBNAIL_EDGE),
            },
            Self::Web | Self::Archive | Self::Lossless => Resize::NONE,
        }
    }

    /// The box the `thumbnail` preset fits into: a 256 px slot on a 2x
    /// display.
    pub const THUMBNAIL_EDGE: u32 = 512;
}

/// Bounds for the resize stage, ADR-0001 D3: fit inside, keep the aspect
/// ratio, never enlarge. This is the geometry only; the resampling is done
/// by the pipeline in `sqzer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Resize {
    /// Widest output allowed, in pixels.
    pub max_width: Option<u32>,
    /// Tallest output allowed, in pixels.
    pub max_height: Option<u32>,
}

impl Resize {
    /// No bounds: every image passes through untouched.
    pub const NONE: Self = Self {
        max_width: None,
        max_height: None,
    };

    /// The size a `width` x `height` image is scaled to, or `None` when it
    /// already fits and is left alone. The tighter bound decides the scale,
    /// the other axis is rounded to the nearest pixel and never drops below
    /// one. A bound of zero is read as one.
    #[must_use]
    pub fn fit(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        let (w, h) = (u64::from(width.max(1)), u64::from(height.max(1)));
        let bound =
            |max: Option<u32>, full: u64| max.map_or(full, |m| u64::from(m.max(1)).min(full));
        let (max_w, max_h) = (bound(self.max_width, w), bound(self.max_height, h));
        if (max_w, max_h) == (w, h) {
            return None;
        }
        // `max_w / w <= max_h / h`, cross-multiplied: the width bound is
        // the tighter one. The rounded axis cannot pass its own bound,
        // because the exact value is at most that bound, an integer.
        let (out_w, out_h) = if max_w * h <= max_h * w {
            (max_w, ((h * max_w + w / 2) / w).max(1))
        } else {
            (((w * max_h + h / 2) / h).max(1), max_h)
        };
        // Both fit in `u32`: neither exceeds the input's own dimension.
        Some((
            u32::try_from(out_w).unwrap_or(width),
            u32::try_from(out_h).unwrap_or(height),
        ))
    }
}

/// Parameters passed to an [`crate::codec::Encoder`].
#[derive(Debug, Clone, PartialEq)]
pub struct EncodeParams {
    /// Quality or lossless. Always resolved before reaching the encoder.
    pub target: Target,
    /// Effort, 0 = fastest, 10 = slowest. Backends clamp to their own range.
    pub effort: u8,
    /// Chroma subsampling, where the codec has the concept.
    pub subsampling: Subsampling,
    /// Keep the ICC profile instead of converting to sRGB.
    pub keep_icc: bool,
    /// Keep EXIF and XMP instead of stripping them. Orientation is applied
    /// and its tag reset either way.
    pub keep_metadata: bool,
    /// Escape hatch for backend-specific knobs. Keys are `codec:name`, e.g.
    /// `jpeg:progressive`. Backends reject keys they own but do not know.
    pub codec_specific: BTreeMap<String, String>,
}

impl Default for EncodeParams {
    fn default() -> Self {
        Self {
            target: Target::Ssimulacra2(70.0),
            effort: 6,
            subsampling: Subsampling::Auto,
            keep_icc: false,
            keep_metadata: false,
            codec_specific: BTreeMap::new(),
        }
    }
}

impl EncodeParams {
    /// The target as something an encoder can act on.
    ///
    /// # Errors
    /// [`Error::InvalidParams`] if the target is still perceptual; the
    /// search loop must resolve it first.
    pub fn resolved(&self) -> Result<Resolved> {
        match self.target {
            Target::Quality(q) => Ok(Resolved::Quality(q.clamp(0.0, 100.0))),
            Target::Lossless => Ok(Resolved::Lossless),
            Target::Ssimulacra2(t) => Err(Error::InvalidParams(format!(
                "perceptual target {t} reached the encoder unresolved"
            ))),
        }
    }

    /// Set a backend-specific option, `codec:key = value`.
    #[must_use]
    pub fn with_codec_opt(mut self, codec: &str, key: &str, value: impl Into<String>) -> Self {
        self.codec_specific
            .insert(format!("{codec}:{key}"), value.into());
        self
    }

    /// Every option addressed to `codec`, with the prefix stripped.
    pub fn codec_opts<'a>(&'a self, codec: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.codec_specific.iter().filter_map(move |(k, v)| {
            k.strip_prefix(codec)
                .and_then(|rest| rest.strip_prefix(':'))
                .map(|key| (key, v.as_str()))
        })
    }
}

/// Parameters passed to a [`crate::codec::Decoder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeOpts {
    /// Refuse images above this many pixels. Decompression-bomb guard.
    pub max_pixels: u64,
    /// Apply EXIF orientation.
    pub apply_orientation: bool,
}

impl Default for DecodeOpts {
    fn default() -> Self {
        Self {
            max_pixels: 268_435_456,
            apply_orientation: true,
        }
    }
}

impl DecodeOpts {
    /// Check header dimensions against `max_pixels` before allocating.
    ///
    /// # Errors
    /// [`Error::TooLarge`] above the limit.
    pub fn check_pixels(&self, width: u32, height: u32) -> Result<()> {
        let pixels = u64::from(width) * u64::from(height);
        if pixels > self.max_pixels {
            return Err(Error::TooLarge {
                pixels,
                limit: self.max_pixels,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perceptual_target_is_not_resolved() {
        assert!(EncodeParams::default().resolved().is_err());
        let p = EncodeParams {
            target: Target::Quality(250.0),
            ..Default::default()
        };
        assert_eq!(p.resolved().unwrap(), Resolved::Quality(100.0));
    }

    #[test]
    fn codec_opts_filter_by_prefix() {
        let p = EncodeParams::default()
            .with_codec_opt("jpeg", "progressive", "false")
            .with_codec_opt("avif", "tune", "ssim");
        let jpeg: Vec<_> = p.codec_opts("jpeg").collect();
        assert_eq!(jpeg, vec![("progressive", "false")]);
    }

    #[test]
    fn web_preset_is_the_default_params() {
        assert_eq!(Preset::Web.params(), EncodeParams::default());
        assert_eq!(Preset::default(), Preset::Web);
    }

    #[test]
    fn preset_names_round_trip() {
        for &p in Preset::ALL {
            assert_eq!(Preset::from_name(p.name()), Some(p));
            assert_eq!(Preset::from_name(&p.name().to_uppercase()), Some(p));
        }
        assert_eq!(Preset::from_name("fast"), None);
        assert_eq!(Preset::Lossless.params().target, Target::Lossless);
    }

    #[test]
    fn only_the_thumbnail_preset_resizes() {
        for &p in Preset::ALL {
            let expected = if p == Preset::Thumbnail {
                Resize {
                    max_width: Some(512),
                    max_height: Some(512),
                }
            } else {
                Resize::NONE
            };
            assert_eq!(p.resize(), expected, "{p:?}");
        }
    }

    #[test]
    fn fit_keeps_the_aspect_ratio_inside_both_bounds() {
        let both = |w, h| Resize {
            max_width: Some(w),
            max_height: Some(h),
        };
        let width = |w| Resize {
            max_width: Some(w),
            max_height: None,
        };
        let height = |h| Resize {
            max_width: None,
            max_height: Some(h),
        };
        assert_eq!(width(1600).fit(4000, 3000), Some((1600, 1200)));
        assert_eq!(height(600).fit(4000, 3000), Some((800, 600)));
        // The tighter bound decides, whichever axis it is on.
        assert_eq!(both(1600, 600).fit(4000, 3000), Some((800, 600)));
        assert_eq!(both(400, 3000).fit(4000, 3000), Some((400, 300)));
        assert_eq!(both(512, 512).fit(3000, 4000), Some((384, 512)));
        // Rounded to nearest, not truncated.
        assert_eq!(width(100).fit(300, 200), Some((100, 67)));
        assert_eq!(width(2).fit(3, 1), Some((2, 1)));
        // A sliver keeps one pixel.
        assert_eq!(width(10).fit(10_000, 3), Some((10, 1)));
        assert_eq!(height(10).fit(3, 10_000), Some((1, 10)));
        assert_eq!(width(0).fit(8, 8), Some((1, 1)));
        // Large inputs do not overflow.
        assert_eq!(
            both(65_535, 65_535).fit(u32::MAX, u32::MAX),
            Some((65_535, 65_535))
        );
    }

    #[test]
    fn fit_never_enlarges() {
        let r = Resize {
            max_width: Some(1600),
            max_height: Some(1600),
        };
        assert_eq!(r.fit(1600, 1200), None);
        assert_eq!(r.fit(640, 480), None);
        assert_eq!(r.fit(1, 1), None);
        assert_eq!(Resize::NONE.fit(4000, 3000), None);
        // One axis inside its bound, the other not: still a downscale.
        assert_eq!(r.fit(3200, 100), Some((1600, 50)));
    }

    #[test]
    fn pixel_budget() {
        let opts = DecodeOpts {
            max_pixels: 100,
            ..Default::default()
        };
        assert!(opts.check_pixels(10, 10).is_ok());
        assert!(opts.check_pixels(10, 11).is_err());
    }
}
