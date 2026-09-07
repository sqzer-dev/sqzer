//! `sqzer` - multi-format image optimizer with best-in-class defaults.
//!
//! ```no_run
//! use sqzer::Sqzer;
//! use sqzer::core::codec::Format;
//! use sqzer::core::params::Target;
//!
//! // Search JPEG quality for a SSIMULACRA2 score of 70.
//! let out = Sqzer::new()
//!     .format(Format::Jpeg)
//!     .target(Target::Ssimulacra2(70.0))
//!     .run(&std::fs::read("photo.png").unwrap())
//!     .unwrap();
//! std::fs::write("photo.jpg", &out.bytes).unwrap();
//! let report = out.report.expect("a perceptual target always reports");
//! println!("quality {} scored {}", report.quality, report.score);
//! ```

pub use sqzer_codecs as codecs;
pub use sqzer_core as core;
pub use sqzer_metrics as metrics;

use std::sync::Arc;

use sqzer_core::Registry;
use sqzer_core::Result;
use sqzer_core::codec::{Encoder, Format, FormatInfo};
use sqzer_core::image::Image;
use sqzer_core::params::{DecodeOpts, EncodeParams, Resolved, Subsampling, Target};
use sqzer_metrics::{Reference, Search, SearchReport, seeds};

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
    /// How the quality was found. `Some` when a perceptual target was
    /// searched, `None` for an explicit quality, lossless, or an encoder
    /// that only writes lossless and so met the target trivially.
    pub report: Option<SearchReport>,
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
    /// A perceptual target runs the SSIMULACRA2 search of `sqzer-metrics`
    /// over the chosen encoder: up to six encodes, each decoded and scored
    /// against the input, starting from the calibrated seed in
    /// [`sqzer_metrics::seeds`] when the backend has one. A target the
    /// encoder cannot reach is not an error; the best candidate is
    /// returned and [`Output::report`] says the target was missed. An
    /// encoder that only writes lossless meets any target with its one
    /// mode and skips the search.
    ///
    /// > **Note**: scoring needs the output format decodable in this
    /// > build. On `wasm32` AVIF is encode-only, so a perceptual target
    /// > for AVIF there returns [`sqzer_core::Error::Unsupported`], and
    /// > the default format falls back to JPEG.
    ///
    /// # Errors
    /// Unknown input, an image over the pixel limit, a decoder failure,
    /// [`sqzer_core::Error::EncoderUnavailable`] for the chosen format, or
    /// [`sqzer_core::Error::Unsupported`] for a perceptual target whose
    /// output this build cannot decode.
    pub fn run(&self, input: &[u8]) -> Result<Output> {
        let decoded = self.registry.decode(input, &self.decode)?;
        let image = &decoded.image;
        let format = self
            .format
            .unwrap_or_else(|| default_format(image, &self.params.target, &self.registry));
        let encoder = self.registry.encoder(format)?;

        let (bytes, target, report) = match self.params.target {
            Target::Ssimulacra2(_) if !encoder.caps().lossy => {
                let params = EncodeParams {
                    target: Target::Lossless,
                    ..self.params.clone()
                };
                (encoder.encode(image, &params)?, Resolved::Lossless, None)
            }
            Target::Ssimulacra2(t) => {
                let mut reference = Reference::new(image)?;
                let found = seeded_search(encoder, t).encode(
                    encoder,
                    image,
                    &self.params,
                    &self.registry,
                    |candidate| reference.score(candidate),
                )?;
                let quality = Resolved::Quality(found.report.quality);
                (found.output, quality, Some(found.report))
            }
            _ => (
                encoder.encode(image, &self.params)?,
                self.params.resolved()?,
                None,
            ),
        };
        Ok(Output {
            bytes,
            format,
            input: decoded.info,
            width: image.width(),
            height: image.height(),
            target,
            report,
        })
    }
}

/// The search for `target` on `encoder`, started from its calibrated seed
/// when there is one and from the midpoint otherwise.
fn seeded_search(encoder: &dyn Encoder, target: f32) -> Search {
    let caps = encoder.caps();
    let mut search = Search::new(target);
    if let Some(seed) = seeds::seed(caps.format, caps.tier, target) {
        search.seed = Some(seed.quality);
        search.seed_step = Some(seed.step);
    }
    search
}

/// Output format when the caller names none. A lossless target keeps PNG,
/// which every build writes. A lossy target goes to AVIF when this build
/// has an encoder for it and, for a perceptual target, a decoder to score
/// its output with; else PNG for transparent input and JPEG for the rest.
/// The content heuristic of ADR-0001 D5 (few colours or hard edges to
/// lossless WebP or PNG, animation to animated WebP or AVIF) comes with
/// the CLI, item 7.
fn default_format(img: &Image, target: &Target, registry: &Registry) -> Format {
    let avif = registry.has_encoder(Format::Avif)
        && (!matches!(target, Target::Ssimulacra2(_)) || registry.has_decoder(Format::Avif));
    match target {
        Target::Lossless => Format::Png,
        _ if avif => Format::Avif,
        _ if img.has_alpha() => Format::Png,
        _ => Format::Jpeg,
    }
}

#[cfg(all(test, feature = "portable"))]
mod tests {
    use super::*;
    use sqzer_core::Error;
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
    fn perceptual_default_runs_the_search() {
        let out = Sqzer::new().run(&png_bytes(ColorType::Rgb)).unwrap();
        assert_eq!(out.format, Format::Avif);
        let report = out.report.expect("a perceptual target reports");
        assert!(
            report.iterations >= 1 && report.iterations <= 6,
            "{report:?}"
        );
        assert!(report.reached, "a flat image is reachable: {report:?}");
        assert_eq!(out.target, Resolved::Quality(report.quality));
        assert_eq!(&out.bytes[4..8], b"ftyp");
    }

    #[test]
    fn explicit_quality_and_lossless_do_not_report() {
        let out = Sqzer::new()
            .target(Target::Quality(80.0))
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert!(out.report.is_none());
        let out = Sqzer::new()
            .target(Target::Lossless)
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert!(out.report.is_none());
    }

    #[test]
    fn lossless_only_encoder_meets_a_perceptual_target_without_a_search() {
        let out = Sqzer::new()
            .format(Format::WebP)
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert_eq!(out.target, Resolved::Lossless);
        assert!(out.report.is_none());
        assert_eq!(&out.bytes[8..12], b"WEBP");
    }

    /// The wasm32 shape: an AVIF encoder with no AVIF decoder.
    fn encode_only_avif() -> Registry {
        let mut narrow = Registry::new();
        narrow.register_decoder(sqzer_codecs::png::PngDecoder);
        narrow.register_decoder(sqzer_codecs::jpeg::JpegDecoder);
        narrow.register_encoder(sqzer_codecs::jpeg::MozjpegEncoder);
        narrow.register_encoder(sqzer_codecs::avif::RavifEncoder);
        narrow
    }

    #[test]
    fn encode_only_format_cannot_take_a_perceptual_target() {
        // Default format steps around it.
        let out = Sqzer::with_registry(encode_only_avif())
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert_eq!(out.format, Format::Jpeg);
        assert!(out.report.is_some());
        // Asking for it by name is refused with the reason.
        let err = Sqzer::with_registry(encode_only_avif())
            .format(Format::Avif)
            .run(&png_bytes(ColorType::Rgb))
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::Unsupported {
                    format: Format::Avif,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn explicit_quality_defaults_to_avif() {
        let out = Sqzer::new()
            .target(Target::Quality(80.0))
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert_eq!(out.format, Format::Avif);
        assert_eq!(out.input.format, Format::Png);
        assert_eq!((out.width, out.height), (4, 4));
        assert_eq!(out.target, Resolved::Quality(80.0));
        assert_eq!(&out.bytes[4..8], b"ftyp");
    }

    #[test]
    fn without_avif_a_lossy_target_falls_back_by_alpha() {
        let mut narrow = Registry::new();
        narrow.register_decoder(sqzer_codecs::png::PngDecoder);
        narrow.register_encoder(sqzer_codecs::png::PngEncoder);
        narrow.register_encoder(sqzer_codecs::jpeg::MozjpegEncoder);
        let sqzer = Sqzer::with_registry(narrow).target(Target::Quality(80.0));
        assert_eq!(
            sqzer.run(&png_bytes(ColorType::Rgb)).unwrap().format,
            Format::Jpeg
        );
        assert_eq!(
            sqzer.run(&png_bytes(ColorType::Rgba)).unwrap().format,
            Format::Png
        );
    }

    #[test]
    fn explicit_format_is_honoured() {
        let out = Sqzer::new()
            .format(Format::WebP)
            .target(Target::Lossless)
            .run(&png_bytes(ColorType::Rgba))
            .unwrap();
        assert_eq!(out.format, Format::WebP);
        assert_eq!(&out.bytes[8..12], b"WEBP");
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
            .format(Format::Jxl)
            .target(Target::Quality(50.0))
            .run(&png_bytes(ColorType::Rgb))
            .unwrap_err();
        assert!(matches!(
            err,
            Error::EncoderUnavailable {
                format: Format::Jxl,
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
