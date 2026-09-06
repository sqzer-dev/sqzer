//! Decoder and encoder traits plus the capability descriptor that lets the
//! pipeline and the CLI reason about a backend without knowing it.

use core::ops::RangeInclusive;

use crate::Result;
use crate::image::Image;
use crate::params::{DecodeOpts, EncodeParams};

/// Image formats `sqzer` knows about. Not every format has an encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
    /// JPEG.
    Jpeg,
    /// PNG.
    Png,
    /// WebP.
    WebP,
    /// AVIF.
    Avif,
    /// JPEG XL.
    Jxl,
    /// GIF.
    Gif,
    /// TIFF.
    Tiff,
    /// HEIC / HEIF.
    Heic,
    /// SVG (input only).
    Svg,
}

/// What a backend was detected as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatInfo {
    /// Container format.
    pub format: Format,
    /// Whether the file has more than one frame.
    pub animated: bool,
}

/// Static description of what an encoder can do.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct EncoderCaps {
    /// Output format.
    pub format: Format,
    /// Supports lossy output.
    pub lossy: bool,
    /// Supports lossless output.
    pub lossless: bool,
    /// Supports an alpha channel.
    pub alpha: bool,
    /// Supports animation.
    pub animation: bool,
    /// Bit depths this encoder accepts.
    pub bit_depth: &'static [u8],
    /// Accepts HDR / float input.
    pub hdr: bool,
    /// Range of the backend's own quality knob, after mapping from 0..=100.
    pub quality_range: RangeInclusive<f32>,
    /// Range of the backend's effort / speed knob.
    pub effort_range: RangeInclusive<u8>,
    /// Which tier this backend belongs to.
    pub tier: Tier,
}

/// Backend tier, mirrors the Cargo feature groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Pure Rust, permissive licence. Always available.
    Portable,
    /// C binding. Opt-in feature.
    Native,
    /// Pure Rust under AGPL. Never a default dependency.
    Agpl,
}

/// A decoder backend.
pub trait Decoder: Send + Sync {
    /// Cheap sniff. Returns `None` if the bytes are not this format.
    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo>;
    /// Full decode.
    ///
    /// # Errors
    /// Malformed input, or an image above `opts.max_pixels`.
    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image>;
}

/// An encoder backend.
pub trait Encoder: Send + Sync {
    /// Static capabilities.
    fn caps(&self) -> &EncoderCaps;
    /// Encode one image.
    ///
    /// # Errors
    /// Unsupported colour type or bit depth for this backend, or a backend failure.
    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>>;
}
