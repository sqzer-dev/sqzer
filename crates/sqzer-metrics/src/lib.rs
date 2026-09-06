//! SSIMULACRA2 scoring and the bisection that turns a perceptual target into
//! an encoder quality value.
//!
//! Planned backend: `fast-ssim2` (BSD-2).

/// Result of one target search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchReport {
    /// Quality the search settled on, 0..=100.
    pub quality: f32,
    /// Score at that quality.
    pub score: f32,
    /// Encodes performed.
    pub iterations: u8,
    /// True if the ceiling was hit without reaching the target.
    pub capped: bool,
}
