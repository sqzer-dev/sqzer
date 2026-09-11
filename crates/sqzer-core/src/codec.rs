//! Decoder and encoder traits plus the capability descriptors that let the
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

impl Format {
    /// Every format, in a stable order.
    pub const ALL: &'static [Self] = &[
        Self::Jpeg,
        Self::Png,
        Self::WebP,
        Self::Avif,
        Self::Jxl,
        Self::Gif,
        Self::Tiff,
        Self::Heic,
        Self::Svg,
    ];

    /// Canonical file extension, without the dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::WebP => "webp",
            Self::Avif => "avif",
            Self::Jxl => "jxl",
            Self::Gif => "gif",
            Self::Tiff => "tiff",
            Self::Heic => "heic",
            Self::Svg => "svg",
        }
    }

    /// MIME type.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
            Self::Avif => "image/avif",
            Self::Jxl => "image/jxl",
            Self::Gif => "image/gif",
            Self::Tiff => "image/tiff",
            Self::Heic => "image/heic",
            Self::Svg => "image/svg+xml",
        }
    }

    /// Parse a file extension or a format name, case-insensitively, with or
    /// without a leading dot.
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        Some(match ext.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::WebP,
            "avif" => Self::Avif,
            "jxl" => Self::Jxl,
            "gif" => Self::Gif,
            "tif" | "tiff" => Self::Tiff,
            "heic" | "heif" => Self::Heic,
            "svg" => Self::Svg,
            _ => return None,
        })
    }

    /// Cargo features of `sqzer-codecs` that provide an encoder for this
    /// format. Empty for input-only formats. This is the `available_in` list
    /// in [`crate::Error::EncoderUnavailable`].
    #[must_use]
    pub const fn encoder_features(self) -> &'static [&'static str] {
        match self {
            Self::Jpeg => &["jpeg", "native-jpegli"],
            Self::Png => &["png"],
            Self::WebP => &["webp-lossless", "native-webp"],
            Self::Avif => &["avif", "native-avif"],
            Self::Jxl => &["native-jxl"],
            Self::Gif | Self::Tiff | Self::Heic | Self::Svg => &[],
        }
    }
}

impl core::fmt::Display for Format {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::WebP => "WebP",
            Self::Avif => "AVIF",
            Self::Jxl => "JPEG XL",
            Self::Gif => "GIF",
            Self::Tiff => "TIFF",
            Self::Heic => "HEIC",
            Self::Svg => "SVG",
        })
    }
}

/// What a backend was detected as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatInfo {
    /// Container format.
    pub format: Format,
    /// Whether the file has more than one frame.
    pub animated: bool,
}

/// Backend tier, mirrors the Cargo feature groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Pure Rust, permissive licence. Always available.
    Portable,
    /// C binding. Opt-in feature.
    Native,
    /// Pure Rust under AGPL. Never a default dependency.
    Agpl,
}

impl core::fmt::Display for Tier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Portable => "portable",
            Self::Native => "native",
            Self::Agpl => "agpl",
        })
    }
}

/// One backend-specific option an encoder accepts through
/// [`EncodeParams::codec_specific`], for `--list-codecs -v` and for
/// checking `--codec-opt` keys before any file is touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecOption {
    /// Key without the `codec:` prefix.
    pub key: &'static str,
    /// Value the backend uses when the option is not set, as the user
    /// would spell it.
    pub default: &'static str,
    /// One line on what it does and which values it takes.
    pub help: &'static str,
}

/// Static description of what a decoder can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderCaps {
    /// Input format.
    pub format: Format,
    /// Backend crate, as `--list-codecs` names it.
    pub name: &'static str,
    /// Reads every frame of an animated file, not just the first.
    pub animation: bool,
    /// Which tier this backend belongs to.
    pub tier: Tier,
}

/// Static description of what an encoder can do.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct EncoderCaps {
    /// Output format.
    pub format: Format,
    /// Backend crate, as `--list-codecs` names it.
    pub name: &'static str,
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
    /// Every `codec_specific` key this backend accepts. A key that is not
    /// listed is rejected by [`Encoder::encode`].
    pub options: &'static [CodecOption],
}

/// A decoder backend.
pub trait Decoder: Send + Sync {
    /// Static capabilities.
    fn caps(&self) -> &DecoderCaps;
    /// Cheap sniff. Returns `None` if the bytes are not this format.
    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo>;
    /// Width and height from the header, without decoding any pixels.
    /// `None` when the header cannot be read or the bytes are not this
    /// format. A batch scheduler uses it to budget memory before deciding
    /// how many files to decode at once.
    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)>;
    /// Full decode. Animated input yields the first frame until animation
    /// is modelled on [`Image`].
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
    /// The image's ICC profile, if any, is embedded as-is. Converting to
    /// sRGB and dropping the profile is the pipeline's job, not the
    /// encoder's.
    ///
    /// # Errors
    /// [`crate::Error::InvalidParams`] if `params.target` is still a
    /// perceptual target, [`crate::Error::Unsupported`] for a colour type,
    /// bit depth or mode this backend cannot do, or a backend failure.
    fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_round_trips() {
        for &f in Format::ALL {
            assert_eq!(Format::from_extension(f.extension()), Some(f));
            assert_eq!(
                Format::from_extension(&format!(".{}", f.extension().to_uppercase())),
                Some(f)
            );
        }
        assert_eq!(Format::from_extension("jpeg"), Some(Format::Jpeg));
        assert_eq!(Format::from_extension("tif"), Some(Format::Tiff));
        assert_eq!(Format::from_extension("bmp"), None);
    }
}
