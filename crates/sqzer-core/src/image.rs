//! The single intermediate image type every decoder produces and every
//! encoder consumes.
//!
//! [`Image`] is deliberately plain: interleaved samples, one of three sample
//! widths, four channel layouts, an optional ICC profile. Orientation is
//! already applied by the decoder. Animation is not modelled yet; it lands
//! with the first animated decoder (GIF, ADR-0001 item 3) so the shape is
//! driven by a real backend rather than guessed.

use std::borrow::Cow;

use crate::codec::Format;
use crate::{Error, Result};

/// Sample layout of an [`Image`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

impl ColorType {
    /// Samples per pixel.
    #[must_use]
    pub const fn channels(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::GrayAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    /// Whether the last channel is alpha.
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::GrayAlpha | Self::Rgba)
    }

    /// The same layout with the alpha channel removed.
    #[must_use]
    pub const fn without_alpha(self) -> Self {
        match self {
            Self::Gray | Self::GrayAlpha => Self::Gray,
            Self::Rgb | Self::Rgba => Self::Rgb,
        }
    }
}

/// Width of one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// 8-bit unsigned.
    U8,
    /// 16-bit unsigned.
    U16,
    /// 32-bit float, linear light.
    F32,
}

impl SampleFormat {
    /// Bits per sample.
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::U8 => 8,
            Self::U16 => 16,
            Self::F32 => 32,
        }
    }
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

impl Samples {
    /// Number of samples, across all channels.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
            Self::F32(v) => v.len(),
        }
    }

    /// True when there are no samples.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Which variant this is.
    #[must_use]
    pub const fn format(&self) -> SampleFormat {
        match self {
            Self::U8(_) => SampleFormat::U8,
            Self::U16(_) => SampleFormat::U16,
            Self::F32(_) => SampleFormat::F32,
        }
    }

    /// The 8-bit buffer, if that is what this is.
    #[must_use]
    pub fn as_u8(&self) -> Option<&[u8]> {
        match self {
            Self::U8(v) => Some(v),
            _ => None,
        }
    }

    /// The 16-bit buffer, if that is what this is.
    #[must_use]
    pub fn as_u16(&self) -> Option<&[u16]> {
        match self {
            Self::U16(v) => Some(v),
            _ => None,
        }
    }

    /// The float buffer, if that is what this is.
    #[must_use]
    pub fn as_f32(&self) -> Option<&[f32]> {
        match self {
            Self::F32(v) => Some(v),
            _ => None,
        }
    }
}

/// A decoded image. Orientation is already applied.
///
/// Invariants, enforced by [`Image::new`]: both dimensions are non-zero and
/// the sample buffer holds exactly `width * height * channels` samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    width: u32,
    height: u32,
    color: ColorType,
    samples: Samples,
    icc: Option<Vec<u8>>,
}

impl Image {
    /// Build an image, checking the invariants.
    ///
    /// # Errors
    /// [`Error::InvalidInput`] on a zero dimension, a pixel count that does
    /// not fit in memory arithmetic, or a sample buffer of the wrong length.
    pub fn new(width: u32, height: u32, color: ColorType, samples: Samples) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidInput(format!(
                "image dimensions must be non-zero, got {width}x{height}"
            )));
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(color.channels()))
            .ok_or_else(|| {
                Error::InvalidInput(format!("{width}x{height} does not fit in memory"))
            })?;
        if samples.len() != expected {
            return Err(Error::InvalidInput(format!(
                "expected {expected} samples for {width}x{height} {color:?}, got {}",
                samples.len()
            )));
        }
        Ok(Self {
            width,
            height,
            color,
            samples,
            icc: None,
        })
    }

    /// Build an 8-bit image.
    ///
    /// # Errors
    /// Same as [`Image::new`].
    pub fn from_u8(width: u32, height: u32, color: ColorType, samples: Vec<u8>) -> Result<Self> {
        Self::new(width, height, color, Samples::U8(samples))
    }

    /// Build a 16-bit image.
    ///
    /// # Errors
    /// Same as [`Image::new`].
    pub fn from_u16(width: u32, height: u32, color: ColorType, samples: Vec<u16>) -> Result<Self> {
        Self::new(width, height, color, Samples::U16(samples))
    }

    /// Attach or remove an ICC profile.
    #[must_use]
    pub fn with_icc(mut self, icc: Option<Vec<u8>>) -> Self {
        self.icc = icc;
        self
    }

    /// Attach or remove an ICC profile in place.
    pub fn set_icc(&mut self, icc: Option<Vec<u8>>) {
        self.icc = icc;
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Channel layout.
    #[must_use]
    pub const fn color(&self) -> ColorType {
        self.color
    }

    /// Interleaved samples, row-major.
    #[must_use]
    pub const fn samples(&self) -> &Samples {
        &self.samples
    }

    /// Embedded ICC profile, if the source had one and it was kept.
    #[must_use]
    pub fn icc(&self) -> Option<&[u8]> {
        self.icc.as_deref()
    }

    /// Total pixel count.
    #[must_use]
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Samples per pixel.
    #[must_use]
    pub const fn channels(&self) -> usize {
        self.color.channels()
    }

    /// Sample width.
    #[must_use]
    pub const fn sample_format(&self) -> SampleFormat {
        self.samples.format()
    }

    /// Whether the last channel is alpha.
    #[must_use]
    pub const fn has_alpha(&self) -> bool {
        self.color.has_alpha()
    }

    /// Take the image apart.
    #[must_use]
    pub fn into_parts(self) -> (u32, u32, ColorType, Samples, Option<Vec<u8>>) {
        (self.width, self.height, self.color, self.samples, self.icc)
    }

    /// The image with 8-bit samples. Borrows when it already is.
    ///
    /// 16-bit samples are rounded to the nearest 8-bit value.
    ///
    /// # Errors
    /// [`Error::Unsupported`] for float samples: mapping linear HDR into
    /// 8-bit sRGB needs a tone-mapping decision that has not been made yet.
    pub fn to_u8(&self, target: Format) -> Result<Cow<'_, Self>> {
        let converted = match &self.samples {
            Samples::U8(_) => return Ok(Cow::Borrowed(self)),
            Samples::U16(v) => v.iter().map(|&s| u16_to_u8(s)).collect(),
            Samples::F32(_) => {
                return Err(Error::Unsupported {
                    format: target,
                    what: "float (HDR) samples".into(),
                });
            }
        };
        Ok(Cow::Owned(Self {
            width: self.width,
            height: self.height,
            color: self.color,
            samples: Samples::U8(converted),
            icc: self.icc.clone(),
        }))
    }

    /// The image with the alpha channel dropped. Borrows when there is none.
    ///
    /// Alpha is discarded, not composited: an encoder without alpha support
    /// gets the colour values exactly as stored.
    #[must_use]
    pub fn without_alpha(&self) -> Cow<'_, Self> {
        if !self.color.has_alpha() {
            return Cow::Borrowed(self);
        }
        let ch = self.channels();
        let samples = match &self.samples {
            Samples::U8(v) => Samples::U8(drop_last_channel(v, ch)),
            Samples::U16(v) => Samples::U16(drop_last_channel(v, ch)),
            Samples::F32(v) => Samples::F32(drop_last_channel(v, ch)),
        };
        Cow::Owned(Self {
            width: self.width,
            height: self.height,
            color: self.color.without_alpha(),
            samples,
            icc: self.icc.clone(),
        })
    }
}

/// Round-to-nearest 16-bit to 8-bit, so that 0xFFFF maps to 0xFF exactly.
#[allow(clippy::cast_possible_truncation)]
fn u16_to_u8(s: u16) -> u8 {
    ((u32::from(s) + 128) / 257) as u8
}

fn drop_last_channel<T: Copy>(v: &[T], channels: usize) -> Vec<T> {
    let mut out = Vec::with_capacity(v.len() / channels * (channels - 1));
    for px in v.chunks_exact(channels) {
        out.extend_from_slice(&px[..channels - 1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_dimension() {
        assert!(Image::from_u8(0, 4, ColorType::Gray, vec![]).is_err());
        assert!(Image::from_u8(4, 0, ColorType::Gray, vec![]).is_err());
    }

    #[test]
    fn rejects_wrong_sample_count() {
        assert!(Image::from_u8(2, 2, ColorType::Rgb, vec![0; 11]).is_err());
        assert!(Image::from_u8(2, 2, ColorType::Rgb, vec![0; 12]).is_ok());
    }

    #[test]
    fn rejects_overflowing_dimensions() {
        assert!(Image::from_u8(u32::MAX, u32::MAX, ColorType::Rgba, vec![]).is_err());
    }

    #[test]
    fn u16_rounds_to_u8() {
        assert_eq!(u16_to_u8(0), 0);
        assert_eq!(u16_to_u8(0xFFFF), 0xFF);
        assert_eq!(u16_to_u8(0x8000), 0x80);
        assert_eq!(u16_to_u8(0x0080), 0);
        assert_eq!(u16_to_u8(0x0081), 1);
    }

    #[test]
    fn to_u8_borrows_when_already_u8() {
        let img = Image::from_u8(1, 1, ColorType::Rgb, vec![1, 2, 3]).unwrap();
        assert!(matches!(img.to_u8(Format::Png).unwrap(), Cow::Borrowed(_)));
    }

    #[test]
    fn to_u8_converts_u16() {
        let img = Image::from_u16(1, 1, ColorType::Gray, vec![0xFFFF]).unwrap();
        let out = img.to_u8(Format::Png).unwrap();
        assert_eq!(out.samples().as_u8(), Some(&[0xFF][..]));
    }

    #[test]
    fn to_u8_refuses_float() {
        let img = Image::new(1, 1, ColorType::Gray, Samples::F32(vec![0.5])).unwrap();
        assert!(matches!(
            img.to_u8(Format::Jpeg),
            Err(Error::Unsupported {
                format: Format::Jpeg,
                ..
            })
        ));
    }

    #[test]
    fn without_alpha_drops_last_channel() {
        let img = Image::from_u8(2, 1, ColorType::Rgba, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let out = img.without_alpha();
        assert_eq!(out.color(), ColorType::Rgb);
        assert_eq!(out.samples().as_u8(), Some(&[1, 2, 3, 5, 6, 7][..]));

        let gray = Image::from_u16(1, 1, ColorType::GrayAlpha, vec![9, 10]).unwrap();
        assert_eq!(gray.without_alpha().samples().as_u16(), Some(&[9][..]));

        let opaque = Image::from_u8(1, 1, ColorType::Rgb, vec![1, 2, 3]).unwrap();
        assert!(matches!(opaque.without_alpha(), Cow::Borrowed(_)));
    }
}
