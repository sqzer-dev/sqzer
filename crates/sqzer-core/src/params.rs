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

/// A named bundle of encode settings, ADR-0001 D4. A preset is a target
/// plus an effort; codec-specific knobs stay in `codec_specific`.
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
    /// Target 60, effort 6. Images that are displayed small.
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
    fn pixel_budget() {
        let opts = DecodeOpts {
            max_pixels: 100,
            ..Default::default()
        };
        assert!(opts.check_pixels(10, 10).is_ok());
        assert!(opts.check_pixels(10, 11).is_err());
    }
}
