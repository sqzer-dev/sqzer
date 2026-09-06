//! Codec-agnostic parameters. Each backend maps these to its own scale.

use std::collections::BTreeMap;

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

/// Parameters passed to an [`crate::codec::Encoder`].
#[derive(Debug, Clone, PartialEq)]
pub struct EncodeParams {
    /// Quality or lossless. Always resolved before reaching the encoder.
    pub target: Target,
    /// Effort, 0 = fastest, 10 = slowest. Backends clamp to their own range.
    pub effort: u8,
    /// Keep the ICC profile instead of converting to sRGB.
    pub keep_icc: bool,
    /// Escape hatch for backend-specific knobs, e.g. `avif:tune=ssim`.
    pub codec_specific: BTreeMap<String, String>,
}

impl Default for EncodeParams {
    fn default() -> Self {
        Self {
            target: Target::Ssimulacra2(70.0),
            effort: 6,
            keep_icc: false,
            codec_specific: BTreeMap::new(),
        }
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
