//! The single intermediate image type every decoder produces and every
//! encoder consumes.

/// Sample layout of an [`Image`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorType {
    /// Single channel.
    Gray,
    /// Gray plus alpha.
    GrayAlpha,
    /// Three channels, sRGB unless an ICC profile says otherwise.
    Rgb,
    /// RGB plus alpha.
    Rgba,
}

/// Sample storage. Decoders pick the smallest type that is lossless for the
/// source; `F32` is reserved for HDR input.
#[derive(Debug, Clone, PartialEq)]
pub enum Samples {
    /// 8 bits per sample.
    U8(Vec<u8>),
    /// 16 bits per sample.
    U16(Vec<u16>),
    /// Linear float samples.
    F32(Vec<f32>),
}

/// A decoded image. Orientation is already applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Channel layout.
    pub color: ColorType,
    /// Interleaved samples, row-major.
    pub samples: Samples,
    /// Embedded ICC profile, if the source had one.
    pub icc: Option<Vec<u8>>,
}

impl Image {
    /// Total pixel count.
    #[must_use]
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}
