//! Perceptual quality metrics. The search loop in `sqzer-metrics` is written
//! against this trait so the core pipeline never depends on a metric crate.

use crate::Result;
use crate::image::Image;

/// A full-reference image quality metric.
pub trait Metric: Send + Sync {
    /// Short name for reports, e.g. `ssimulacra2`.
    fn name(&self) -> &'static str;
    /// Score `distorted` against `reference`. Higher is better; the scale is
    /// the metric's own (SSIMULACRA2: 100 is identical, negative is possible).
    ///
    /// # Errors
    /// Mismatched dimensions, or a sample format the metric cannot take.
    fn score(&self, reference: &Image, distorted: &Image) -> Result<f32>;
}
