//! The single intermediate image type every decoder produces and every
//! encoder consumes.
//!
//! [`Image`] is deliberately plain: interleaved samples, one of three sample
//! widths, four channel layouts, and the blobs a container carries beside
//! the pixels ([`Metadata`]: ICC, EXIF, XMP). Orientation is already applied
//! by the decoder; [`Image::apply_orientation`] is the shared implementation
//! decoders use for that, and it resets the EXIF orientation tag as it
//! goes, so kept EXIF never contradicts the pixels. Animation is not modelled yet; it
//! lands with the first animated decoder (GIF) so the shape is driven by a
//! real backend rather than guessed.

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

/// EXIF / TIFF orientation, the transform that maps stored samples to the
/// picture the author intended. Numbering follows TIFF tag 0x0112.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Orientation {
    /// Stored as displayed.
    #[default]
    Normal = 1,
    /// Mirrored left to right.
    FlipHorizontal = 2,
    /// Rotated 180 degrees.
    Rotate180 = 3,
    /// Mirrored top to bottom.
    FlipVertical = 4,
    /// Mirrored along the top-left to bottom-right diagonal.
    Transpose = 5,
    /// Rotated 90 degrees clockwise.
    Rotate90 = 6,
    /// Mirrored along the top-right to bottom-left diagonal.
    Transverse = 7,
    /// Rotated 270 degrees clockwise.
    Rotate270 = 8,
}

impl Orientation {
    /// From the raw EXIF tag value. `None` for anything outside `1..=8`.
    #[must_use]
    pub const fn from_exif(value: u32) -> Option<Self> {
        Some(match value {
            1 => Self::Normal,
            2 => Self::FlipHorizontal,
            3 => Self::Rotate180,
            4 => Self::FlipVertical,
            5 => Self::Transpose,
            6 => Self::Rotate90,
            7 => Self::Transverse,
            8 => Self::Rotate270,
            _ => return None,
        })
    }

    /// Whether applying this swaps width and height.
    #[must_use]
    pub const fn swaps_axes(self) -> bool {
        matches!(
            self,
            Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270
        )
    }

    /// The single orientation that applies `self` first and `next` after
    /// it, so a container listing several transforms in order, as HEIF's
    /// `irot` and `imir` properties are, composes into one
    /// [`Image::apply_orientation`] call.
    #[must_use]
    pub const fn then(self, next: Self) -> Self {
        let (swap1, fx1, fy1) = self.parts();
        let (swap2, fx2, fy2) = next.parts();
        // Moving the second swap past the first pair of flips exchanges
        // which axis each of those flips acts on.
        let (fx1, fy1) = if swap2 { (fy1, fx1) } else { (fx1, fy1) };
        Self::from_parts(swap1 != swap2, fx1 != fx2, fy1 != fy2)
    }

    /// The orientation as a transpose followed by flips of the output's
    /// x and y axes: `(swap, flip_x, flip_y)`.
    const fn parts(self) -> (bool, bool, bool) {
        match self {
            Self::Normal => (false, false, false),
            Self::FlipHorizontal => (false, true, false),
            Self::Rotate180 => (false, true, true),
            Self::FlipVertical => (false, false, true),
            Self::Transpose => (true, false, false),
            Self::Rotate90 => (true, true, false),
            Self::Transverse => (true, true, true),
            Self::Rotate270 => (true, false, true),
        }
    }

    const fn from_parts(swap: bool, flip_x: bool, flip_y: bool) -> Self {
        match (swap, flip_x, flip_y) {
            (false, false, false) => Self::Normal,
            (false, true, false) => Self::FlipHorizontal,
            (false, true, true) => Self::Rotate180,
            (false, false, true) => Self::FlipVertical,
            (true, false, false) => Self::Transpose,
            (true, true, false) => Self::Rotate90,
            (true, true, true) => Self::Transverse,
            (true, false, true) => Self::Rotate270,
        }
    }

    /// Every orientation, in EXIF order.
    pub const ALL: [Self; 8] = [
        Self::Normal,
        Self::FlipHorizontal,
        Self::Rotate180,
        Self::FlipVertical,
        Self::Transpose,
        Self::Rotate90,
        Self::Transverse,
        Self::Rotate270,
    ];
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

/// The blobs a container carries beside the pixels. Decoders attach what
/// they find; the pipeline decides what the encoder gets (ADR-0001 D7:
/// ICC converted to sRGB and dropped, EXIF and XMP stripped, unless asked
/// otherwise).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Metadata {
    /// ICC profile, the bytes as embedded.
    pub icc: Option<Vec<u8>>,
    /// EXIF as a TIFF structure starting at the byte-order mark (`II` or
    /// `MM`), without the `Exif\0\0` prefix JPEG uses. [`Image::with_exif`]
    /// strips that prefix.
    pub exif: Option<Vec<u8>>,
    /// XMP packet, the XML bytes.
    pub xmp: Option<Vec<u8>>,
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
    meta: Metadata,
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
            meta: Metadata::default(),
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
        self.meta.icc = icc;
        self
    }

    /// Attach or remove an ICC profile in place.
    pub fn set_icc(&mut self, icc: Option<Vec<u8>>) {
        self.meta.icc = icc;
    }

    /// Attach or remove EXIF. A leading `Exif\0\0` is stripped so the blob
    /// starts at the TIFF header whatever container it came from.
    #[must_use]
    pub fn with_exif(mut self, exif: Option<Vec<u8>>) -> Self {
        self.meta.exif = exif.map(|raw| match raw.strip_prefix(EXIF_PREFIX) {
            Some(tiff) => tiff.to_vec(),
            None => raw,
        });
        self
    }

    /// Attach or remove an XMP packet.
    #[must_use]
    pub fn with_xmp(mut self, xmp: Option<Vec<u8>>) -> Self {
        self.meta.xmp = xmp;
        self
    }

    /// Replace every blob at once.
    #[must_use]
    pub fn with_metadata(mut self, meta: Metadata) -> Self {
        self.meta = meta;
        self
    }

    /// Drop EXIF and XMP, keeping the ICC profile: the default metadata
    /// policy.
    pub fn strip_metadata(&mut self) {
        self.meta.exif = None;
        self.meta.xmp = None;
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
        self.meta.icc.as_deref()
    }

    /// EXIF, as a TIFF structure from the byte-order mark on.
    #[must_use]
    pub fn exif(&self) -> Option<&[u8]> {
        self.meta.exif.as_deref()
    }

    /// XMP packet.
    #[must_use]
    pub fn xmp(&self) -> Option<&[u8]> {
        self.meta.xmp.as_deref()
    }

    /// Every blob carried beside the pixels.
    #[must_use]
    pub const fn metadata(&self) -> &Metadata {
        &self.meta
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

    /// The image with `orientation` applied, so that it displays upright
    /// with no further metadata: an EXIF blob on the image gets its
    /// `Orientation` tag reset to 1 in the same step. Returns `self`
    /// untouched for [`Orientation::Normal`].
    #[must_use]
    pub fn apply_orientation(mut self, orientation: Orientation) -> Self {
        if orientation == Orientation::Normal {
            return self;
        }
        if let Some(exif) = &mut self.meta.exif {
            reset_exif_orientation(exif);
        }
        let (width, height) = if orientation.swaps_axes() {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let (w, h) = (self.width as usize, self.height as usize);
        // For every output pixel, where in the stored image it comes from.
        let source = |x: usize, y: usize| -> (usize, usize) {
            match orientation {
                Orientation::Normal => (x, y),
                Orientation::FlipHorizontal => (w - 1 - x, y),
                Orientation::Rotate180 => (w - 1 - x, h - 1 - y),
                Orientation::FlipVertical => (x, h - 1 - y),
                Orientation::Transpose => (y, x),
                Orientation::Rotate90 => (y, h - 1 - x),
                Orientation::Transverse => (w - 1 - y, h - 1 - x),
                Orientation::Rotate270 => (w - 1 - y, x),
            }
        };
        let out = (width as usize, height as usize, self.channels(), w);
        let samples = match &self.samples {
            Samples::U8(v) => Samples::U8(remap(v, out, source)),
            Samples::U16(v) => Samples::U16(remap(v, out, source)),
            Samples::F32(v) => Samples::F32(remap(v, out, source)),
        };
        Self {
            width,
            height,
            color: self.color,
            samples,
            meta: self.meta,
        }
    }

    /// Take the image apart.
    #[must_use]
    pub fn into_parts(self) -> (u32, u32, ColorType, Samples, Metadata) {
        (self.width, self.height, self.color, self.samples, self.meta)
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
            meta: self.meta.clone(),
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
            meta: self.meta.clone(),
        })
    }
}

/// Gather pixels of a `(width, height, channels, source_width)` output from
/// an interleaved buffer, `source` mapping output to source coordinates.
fn remap<T: Copy>(
    v: &[T],
    (width, height, channels, source_width): (usize, usize, usize, usize),
    source: impl Fn(usize, usize) -> (usize, usize),
) -> Vec<T> {
    let mut out = Vec::with_capacity(v.len());
    for y in 0..height {
        for x in 0..width {
            let (sx, sy) = source(x, y);
            let at = (sy * source_width + sx) * channels;
            out.extend_from_slice(&v[at..at + channels]);
        }
    }
    out
}

/// What JPEG puts in front of the TIFF structure in its APP1 segment.
const EXIF_PREFIX: &[u8] = b"Exif\0\0";

/// Set the `Orientation` entry of the first IFD to 1, in place. Only an
/// entry of the standard shape (type SHORT, count 1) is touched; anything
/// else, including a blob that does not parse, is left alone. Returns
/// whether an entry was rewritten.
pub fn reset_exif_orientation(exif: &mut [u8]) -> bool {
    // Offsets come from the blob, so every sum is checked: on a 32-bit
    // target a hostile IFD offset would otherwise overflow `usize`.
    let read_u16 = |bytes: &[u8], at: usize, big: bool| -> Option<u16> {
        let b: [u8; 2] = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let read_u32 = |bytes: &[u8], at: usize, big: bool| -> Option<u32> {
        let b: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    let big = match exif.get(..2) {
        Some(b"MM") => true,
        Some(b"II") => false,
        _ => return false,
    };
    if read_u16(exif, 2, big) != Some(42) {
        return false;
    }
    let Some(ifd) = read_u32(exif, 4, big).and_then(|o| usize::try_from(o).ok()) else {
        return false;
    };
    let Some(entries) = read_u16(exif, ifd, big) else {
        return false;
    };
    for i in 0..usize::from(entries) {
        let Some(at) = ifd.checked_add(2 + i * 12) else {
            return false;
        };
        let entry = (
            read_u16(exif, at, big),
            read_u16(exif, at + 2, big),
            read_u32(exif, at + 4, big),
        );
        if entry == (Some(0x0112), Some(3), Some(1)) {
            // A SHORT sits in the first two bytes of the value slot, and
            // the reads above proved the entry is in bounds.
            let value = if big {
                1u16.to_be_bytes()
            } else {
                1u16.to_le_bytes()
            };
            exif[at + 8..at + 10].copy_from_slice(&value);
            return true;
        }
    }
    false
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
    #[test]
    fn orientation_from_exif_bounds() {
        assert_eq!(Orientation::from_exif(1), Some(Orientation::Normal));
        assert_eq!(Orientation::from_exif(8), Some(Orientation::Rotate270));
        assert_eq!(Orientation::from_exif(0), None);
        assert_eq!(Orientation::from_exif(9), None);
    }

    /// 3x2 gray image, one distinct value per pixel:
    /// ```text
    /// 1 2 3
    /// 4 5 6
    /// ```
    fn six() -> Image {
        Image::from_u8(3, 2, ColorType::Gray, vec![1, 2, 3, 4, 5, 6]).unwrap()
    }

    #[test]
    fn orientation_normal_is_identity() {
        assert_eq!(six().apply_orientation(Orientation::Normal), six());
    }

    #[test]
    fn orientation_flips_and_rotations() {
        let cases: [(Orientation, (u32, u32), &[u8]); 7] = [
            (Orientation::FlipHorizontal, (3, 2), &[3, 2, 1, 6, 5, 4]),
            (Orientation::Rotate180, (3, 2), &[6, 5, 4, 3, 2, 1]),
            (Orientation::FlipVertical, (3, 2), &[4, 5, 6, 1, 2, 3]),
            (Orientation::Transpose, (2, 3), &[1, 4, 2, 5, 3, 6]),
            (Orientation::Rotate90, (2, 3), &[4, 1, 5, 2, 6, 3]),
            (Orientation::Transverse, (2, 3), &[6, 3, 5, 2, 4, 1]),
            (Orientation::Rotate270, (2, 3), &[3, 6, 2, 5, 1, 4]),
        ];
        for (o, (w, h), expected) in cases {
            let out = six().apply_orientation(o);
            assert_eq!((out.width(), out.height()), (w, h), "{o:?} size");
            assert_eq!(out.samples().as_u8(), Some(expected), "{o:?} samples");
        }
    }

    #[test]
    fn orientation_composes_like_two_applications() {
        // `six()` is 3 x 2 with distinct samples, so every one of the 64
        // pairs is told apart from every other transform.
        for a in Orientation::ALL {
            for b in Orientation::ALL {
                let twice = six().apply_orientation(a).apply_orientation(b);
                let once = six().apply_orientation(a.then(b));
                assert_eq!(twice, once, "{a:?} then {b:?}");
            }
            assert_eq!(a.then(Orientation::Normal), a);
            assert_eq!(Orientation::Normal.then(a), a);
        }
        // HEIF's iPhone case: `irot` 90 degrees clockwise on its own.
        assert_eq!(
            Orientation::Normal.then(Orientation::Rotate90),
            Orientation::Rotate90
        );
        assert_eq!(
            Orientation::Rotate90.then(Orientation::Rotate90),
            Orientation::Rotate180
        );
        assert_eq!(
            Orientation::Rotate90.then(Orientation::FlipHorizontal),
            Orientation::Transpose
        );
        assert_eq!(
            Orientation::Rotate90.then(Orientation::FlipVertical),
            Orientation::Transverse
        );
    }

    /// A minimal TIFF with one IFD holding `Orientation = value` and an
    /// `Artist` entry after it, in either byte order.
    fn tiff(value: u16, big: bool) -> Vec<u8> {
        let u16b = |v: u16| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let u32b = |v: u32| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut t = Vec::new();
        t.extend_from_slice(if big { b"MM" } else { b"II" });
        t.extend_from_slice(&u16b(42));
        t.extend_from_slice(&u32b(8));
        t.extend_from_slice(&u16b(2));
        // Artist first, so the walk has to skip an entry.
        t.extend_from_slice(&u16b(0x013B));
        t.extend_from_slice(&u16b(2));
        t.extend_from_slice(&u32b(4));
        t.extend_from_slice(b"me\0\0");
        t.extend_from_slice(&u16b(0x0112));
        t.extend_from_slice(&u16b(3));
        t.extend_from_slice(&u32b(1));
        t.extend_from_slice(&u16b(value));
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&u32b(0));
        t
    }

    #[test]
    fn exif_orientation_is_reset_when_applied() {
        for big in [false, true] {
            let img = Image::from_u8(3, 2, ColorType::Gray, vec![1, 2, 3, 4, 5, 6])
                .unwrap()
                .with_exif(Some(tiff(6, big)));
            let out = img.apply_orientation(Orientation::Rotate90);
            assert_eq!(out.exif(), Some(&tiff(1, big)[..]), "big endian {big}");
            // Normal leaves the blob alone, tag included.
            let img = Image::from_u8(1, 1, ColorType::Gray, vec![1])
                .unwrap()
                .with_exif(Some(tiff(6, big)));
            assert_eq!(
                img.apply_orientation(Orientation::Normal).exif(),
                Some(&tiff(6, big)[..])
            );
        }
        let mut junk = b"not a tiff".to_vec();
        assert!(!reset_exif_orientation(&mut junk));
        assert_eq!(junk, b"not a tiff");
        // An IFD offset past the end, or one that would wrap `usize`.
        for offset in [0x0000_1000u32, u32::MAX - 1] {
            let mut hostile = b"II\x2a\x00".to_vec();
            hostile.extend_from_slice(&offset.to_le_bytes());
            hostile.extend_from_slice(&[0xFF; 16]);
            let before = hostile.clone();
            assert!(!reset_exif_orientation(&mut hostile));
            assert_eq!(hostile, before);
        }
    }

    #[test]
    fn exif_prefix_is_stripped_and_strip_metadata_keeps_icc() {
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend_from_slice(&tiff(1, false));
        let mut img = Image::from_u8(1, 1, ColorType::Gray, vec![1])
            .unwrap()
            .with_exif(Some(prefixed))
            .with_xmp(Some(b"<x/>".to_vec()))
            .with_icc(Some(vec![9]));
        assert_eq!(img.exif(), Some(&tiff(1, false)[..]));
        assert_eq!(img.metadata().xmp.as_deref(), Some(&b"<x/>"[..]));
        img.strip_metadata();
        assert_eq!(
            (img.exif(), img.xmp(), img.icc()),
            (None, None, Some(&[9u8][..]))
        );
        // The adapters carry the blobs.
        let img = Image::from_u16(1, 1, ColorType::GrayAlpha, vec![1, 2])
            .unwrap()
            .with_xmp(Some(b"<x/>".to_vec()));
        assert_eq!(img.to_u8(Format::Png).unwrap().xmp(), Some(&b"<x/>"[..]));
        assert_eq!(img.without_alpha().xmp(), Some(&b"<x/>"[..]));
    }

    #[test]
    fn orientation_keeps_channels_together_and_icc() {
        let img = Image::from_u16(2, 1, ColorType::Rgb, vec![1, 2, 3, 4, 5, 6])
            .unwrap()
            .with_icc(Some(vec![9]));
        let out = img.apply_orientation(Orientation::Rotate90);
        assert_eq!((out.width(), out.height()), (1, 2));
        assert_eq!(out.samples().as_u16(), Some(&[1, 2, 3, 4, 5, 6][..]));
        assert_eq!(out.icc(), Some(&[9][..]));
        let f = Image::new(2, 1, ColorType::Gray, Samples::F32(vec![0.25, 0.75])).unwrap();
        assert_eq!(
            f.apply_orientation(Orientation::FlipHorizontal)
                .samples()
                .as_f32(),
            Some(&[0.75, 0.25][..])
        );
    }
}
