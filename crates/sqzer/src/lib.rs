//! `sqzer` - multi-format image optimizer with best-in-class defaults.
//!
//! ```no_run
//! use sqzer::Sqzer;
//! use sqzer::core::codec::Format;
//! use sqzer::core::params::Target;
//!
//! let out = Sqzer::new()
//!     .format(Format::Jpeg)
//!     .target(Target::Quality(80.0))
//!     .run(&std::fs::read("photo.png").unwrap())
//!     .unwrap();
//! std::fs::write("photo.jpg", &out.bytes).unwrap();
//! ```

pub use sqzer_codecs as codecs;
pub use sqzer_core as core;
pub use sqzer_metrics as metrics;

use std::sync::Arc;

use sqzer_core::Registry;
use sqzer_core::codec::{Format, FormatInfo};
use sqzer_core::image::Image;
use sqzer_core::params::{DecodeOpts, EncodeParams, Resolved, Subsampling, Target};
use sqzer_core::{Error, Result};

/// One-shot builder. Cheap to create; holds no image data.
#[derive(Clone)]
pub struct Sqzer {
    format: Option<Format>,
    params: EncodeParams,
    decode: DecodeOpts,
    registry: Arc<Registry>,
}

/// What [`Sqzer::run`] produces.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// Encoded file.
    pub bytes: Vec<u8>,
    /// Format of `bytes`.
    pub format: Format,
    /// What the input was detected as.
    pub input: FormatInfo,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// The target the encoder actually ran with.
    pub target: Resolved,
}

impl Default for Sqzer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Sqzer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sqzer")
            .field("format", &self.format)
            .field("params", &self.params)
            .field("decode", &self.decode)
            .field("registry", &self.registry)
            .finish()
    }
}

impl Sqzer {
    /// Builder with the `web` preset over every backend in this build.
    #[must_use]
    pub fn new() -> Self {
        Self::with_registry(sqzer_codecs::registry())
    }

    /// Builder over a caller-assembled registry, for adding backends the
    /// library does not ship (the AGPL tier) or removing ones it does.
    #[must_use]
    pub fn with_registry(registry: Registry) -> Self {
        Self {
            format: None,
            params: EncodeParams::default(),
            decode: DecodeOpts::default(),
            registry: Arc::new(registry),
        }
    }

    /// The backends this builder dispatches over.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Output format. Defaults to a content-aware choice.
    #[must_use]
    pub fn format(mut self, f: Format) -> Self {
        self.format = Some(f);
        self
    }

    /// Perceptual target or explicit quality.
    #[must_use]
    pub fn target(mut self, t: Target) -> Self {
        self.params.target = t;
        self
    }

    /// Effort, 0 = fastest, 10 = slowest.
    #[must_use]
    pub fn effort(mut self, effort: u8) -> Self {
        self.params.effort = effort.min(10);
        self
    }

    /// Chroma subsampling, for codecs that have it.
    #[must_use]
    pub fn subsampling(mut self, s: Subsampling) -> Self {
        self.params.subsampling = s;
        self
    }

    /// A backend-specific knob, e.g. `("jpeg", "progressive", "false")`.
    #[must_use]
    pub fn codec_opt(mut self, codec: &str, key: &str, value: impl Into<String>) -> Self {
        self.params = self.params.with_codec_opt(codec, key, value);
        self
    }

    /// Refuse inputs above this many pixels.
    #[must_use]
    pub fn max_pixels(mut self, max: u64) -> Self {
        self.decode.max_pixels = max;
        self
    }

    /// Decode, transform, encode. No resize or colour management yet.
    ///
    /// > **Note**: the SSIMULACRA2 search is not built yet (ADR-0001 item
    /// > 5), so the default perceptual target cannot be honoured. Until it
    /// > lands, pass an explicit [`Target::Quality`] or [`Target::Lossless`];
    /// > a perceptual target returns [`Error::InvalidParams`] rather than
    /// > quietly picking a number.
    ///
    /// # Errors
    /// Unknown input, an image over the pixel limit, a decoder failure,
    /// [`Error::EncoderUnavailable`] for the chosen format, or
    /// [`Error::InvalidParams`] for a perceptual target.
    pub fn run(&self, input: &[u8]) -> Result<Output> {
        let decoded = self.registry.decode(input, &self.decode)?;
        let format = self
            .format
            .unwrap_or_else(|| default_format(&decoded.image));
        let encoder = self.registry.encoder(format)?;

        let target = match self.params.target {
            Target::Ssimulacra2(t) => {
                return Err(Error::InvalidParams(format!(
                    "perceptual target {t} needs the SSIMULACRA2 search, which is not in \
                     this build yet; pass Target::Quality or Target::Lossless"
                )));
            }
            _ => self.params.resolved()?,
        };
        let bytes = encoder.encode(&decoded.image, &self.params)?;
        Ok(Output {
            bytes,
            format,
            input: decoded.info,
            width: decoded.image.width(),
            height: decoded.image.height(),
            target,
        })
    }
}

/// Placeholder for the content-aware choice in ADR-0001 D5: transparency
/// keeps PNG, everything else goes to JPEG. Becomes AVIF for photographic
/// input once an AVIF encoder exists (item 4).
fn default_format(img: &Image) -> Format {
    if img.has_alpha() {
        Format::Png
    } else {
        Format::Jpeg
    }
}

#[cfg(all(test, feature = "portable"))]
mod tests {
    use super::*;
    use sqzer_core::image::ColorType;

    fn png_bytes(color: ColorType) -> Vec<u8> {
        let img = Image::from_u8(4, 4, color, vec![200; 16 * color.channels()]).unwrap();
        sqzer_codecs::registry()
            .encoder(Format::Png)
            .unwrap()
            .encode(
                &img,
                &EncodeParams {
                    target: Target::Lossless,
                    ..Default::default()
                },
            )
            .unwrap()
    }

    #[test]
    fn perceptual_default_is_refused_until_the_search_exists() {
        let err = Sqzer::new().run(&png_bytes(ColorType::Rgb)).unwrap_err();
        assert!(matches!(err, Error::InvalidParams(_)), "{err}");
    }

    #[test]
    fn explicit_quality_converts_png_to_jpeg() {
        let out = Sqzer::new()
            .target(Target::Quality(80.0))
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert_eq!(out.format, Format::Jpeg);
        assert_eq!(out.input.format, Format::Png);
        assert_eq!((out.width, out.height), (4, 4));
        assert_eq!(out.target, Resolved::Quality(80.0));
        assert_eq!(&out.bytes[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn transparent_input_defaults_to_png() {
        let out = Sqzer::new()
            .target(Target::Lossless)
            .run(&png_bytes(ColorType::Rgba))
            .unwrap();
        assert_eq!(out.format, Format::Png);
        assert!(out.bytes.starts_with(b"\x89PNG"));
    }

    #[test]
    fn missing_encoder_is_an_error_not_a_fallback() {
        let err = Sqzer::new()
            .format(Format::Avif)
            .target(Target::Quality(50.0))
            .run(&png_bytes(ColorType::Rgb))
            .unwrap_err();
        assert!(matches!(
            err,
            Error::EncoderUnavailable {
                format: Format::Avif,
                ..
            }
        ));
    }

    #[test]
    fn unknown_input_is_reported() {
        assert!(matches!(
            Sqzer::new().run(b"definitely not an image"),
            Err(Error::UnknownFormat)
        ));
    }

    #[test]
    fn custom_registry_is_honoured() {
        let empty = Sqzer::with_registry(Registry::new());
        assert!(matches!(
            empty.run(&png_bytes(ColorType::Rgb)),
            Err(Error::UnknownFormat)
        ));
    }
}
