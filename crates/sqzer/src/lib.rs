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
//!
//! The pipeline is ADR-0001 D3: [`Sqzer::decode`], [`Sqzer::transform`],
//! [`Sqzer::encode`]. [`Sqzer::run`] is the three in order.

pub use sqzer_codecs as codecs;
pub use sqzer_core as core;
pub use sqzer_metrics as metrics;

mod resize;

use std::sync::Arc;

use sqzer_core::codec::{Encoder, Format, FormatInfo, Tier};
use sqzer_core::content::{self, Content};
use sqzer_core::image::Image;
use sqzer_core::params::{DecodeOpts, EncodeParams, Preset, Resize, Resolved, Subsampling, Target};
use sqzer_core::{Decoded, Error, Registry, Result};
use sqzer_metrics::{Reference, Search, SearchReport, seeds};

/// One-shot builder. Cheap to create; holds no image data.
#[derive(Clone)]
pub struct Sqzer {
    format: Option<Format>,
    params: EncodeParams,
    decode: DecodeOpts,
    resize: Resize,
    fast: bool,
    registry: Arc<Registry>,
}

/// A step of [`Sqzer::encode_with`], reported as it happens so a caller
/// can show live progress. More variants may arrive; match with a
/// wildcard.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Progress {
    /// The perceptual search scored one trial. `n` counts from one, `max`
    /// is the encode budget.
    Trial {
        /// Position in the budget.
        n: u8,
        /// The budget.
        max: u8,
        /// Quality tried.
        quality: f32,
        /// Score it reached.
        score: f32,
    },
}

/// What [`Sqzer::run`] produces.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// Encoded file.
    pub bytes: Vec<u8>,
    /// Format of `bytes`.
    pub format: Format,
    /// The backend that wrote `bytes`, by crate name.
    pub backend: &'static str,
    /// The tier that backend belongs to.
    pub tier: Tier,
    /// What the input was detected as.
    pub input: FormatInfo,
    /// What the input looks like. Decides the default format.
    pub content: Content,
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
            .field("resize", &self.resize)
            .field("fast", &self.fast)
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
            resize: Resize::NONE,
            fast: false,
            registry: Arc::new(registry),
        }
    }

    /// The backends this builder dispatches over.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The encode parameters as currently configured.
    #[must_use]
    pub fn params(&self) -> &EncodeParams {
        &self.params
    }

    /// The decode options as currently configured.
    #[must_use]
    pub fn decode_opts(&self) -> &DecodeOpts {
        &self.decode
    }

    /// The resize bounds as currently configured.
    #[must_use]
    pub fn resize_bounds(&self) -> Resize {
        self.resize
    }

    /// The output format as currently configured. `None` means a
    /// content-aware default is chosen per image.
    #[must_use]
    pub fn format_choice(&self) -> Option<Format> {
        self.format
    }

    /// Start from a preset: its target, effort and resize bounds replace
    /// the current ones, everything else is kept. Call it before the
    /// flags that should override it.
    #[must_use]
    pub fn preset(mut self, preset: Preset) -> Self {
        let p = preset.params();
        self.params.target = p.target;
        self.params.effort = p.effort;
        self.resize = preset.resize();
        self
    }

    /// Scale down to at most this many pixels wide, keeping the aspect
    /// ratio. Never enlarges. With [`Sqzer::max_height`] the image fits
    /// inside both.
    #[must_use]
    pub fn max_width(mut self, pixels: u32) -> Self {
        self.resize.max_width = Some(pixels);
        self
    }

    /// Scale down to at most this many pixels tall, keeping the aspect
    /// ratio. Never enlarges.
    #[must_use]
    pub fn max_height(mut self, pixels: u32) -> Self {
        self.resize.max_height = Some(pixels);
        self
    }

    /// Both resize bounds at once, replacing the current ones.
    /// [`Resize::NONE`] turns the stage off, for example after a preset
    /// that set it.
    #[must_use]
    pub fn resize(mut self, bounds: Resize) -> Self {
        self.resize = bounds;
        self
    }

    /// Skip the perceptual search: encode once at the calibrated seed
    /// quality for the target. Needs a seed table for the backend.
    #[must_use]
    pub fn fast(mut self, fast: bool) -> Self {
        self.fast = fast;
        self
    }

    /// Keep the ICC profile on the output instead of converting to sRGB.
    #[must_use]
    pub fn keep_icc(mut self, keep: bool) -> Self {
        self.params.keep_icc = keep;
        self
    }

    /// Apply EXIF orientation while decoding. On by default.
    #[must_use]
    pub fn auto_orient(mut self, apply: bool) -> Self {
        self.decode.apply_orientation = apply;
        self
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

    /// Decode, transform, encode: [`Sqzer::decode`], [`Sqzer::transform`],
    /// [`Sqzer::encode`]. No colour management yet.
    ///
    /// A perceptual target runs the SSIMULACRA2 search of `sqzer-metrics`
    /// over the chosen encoder: up to six encodes, each decoded and scored
    /// against the transformed input, starting from the calibrated seed in
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
    /// Unknown input, input this build recognises but cannot decode
    /// ([`sqzer_core::Error::DecoderUnavailable`], naming the feature or
    /// the missing library), an image over the pixel limit, a decoder
    /// failure, a zero resize bound, [`sqzer_core::Error::EncoderUnavailable`]
    /// for the chosen format, or [`sqzer_core::Error::Unsupported`] for a perceptual
    /// target whose output this build cannot decode.
    pub fn run(&self, input: &[u8]) -> Result<Output> {
        self.encode(&self.transform(self.decode(input)?)?)
    }

    /// Probe and decode `input` with the configured decode options.
    ///
    /// # Errors
    /// [`sqzer_core::Error::UnknownFormat`],
    /// [`sqzer_core::Error::DecoderUnavailable`],
    /// [`sqzer_core::Error::TooLarge`] or the decoder's own error.
    pub fn decode(&self, input: &[u8]) -> Result<Decoded> {
        self.registry.decode(input, &self.decode)
    }

    /// The stage between decode and encode, ADR-0001 D3. Today that is
    /// the resize: fit inside [`Sqzer::max_width`] and
    /// [`Sqzer::max_height`], aspect ratio kept, never enlarged, Lanczos3
    /// in linear light with premultiplied alpha. Orientation was applied
    /// by the decoder, so the bounds are those of the picture as displayed.
    /// An image that already fits is returned as it came.
    ///
    /// The result is what the encoder sees and what a perceptual target is
    /// scored against.
    ///
    /// # Errors
    /// [`sqzer_core::Error::InvalidParams`] for a bound of zero,
    /// [`sqzer_core::Error::Transform`] if the resampler refuses the image.
    pub fn transform(&self, decoded: Decoded) -> Result<Decoded> {
        if self.resize.max_width == Some(0) || self.resize.max_height == Some(0) {
            return Err(Error::InvalidParams(
                "a resize bound must be at least one pixel".into(),
            ));
        }
        Ok(Decoded {
            image: resize::fit(decoded.image, self.resize)?,
            info: decoded.info,
        })
    }

    /// Encode an already decoded image, as given: the resize is
    /// [`Sqzer::transform`]'s, not this method's. Everything
    /// [`Sqzer::run`] says about targets and errors applies; a caller that
    /// wants several output formats from one input decodes and transforms
    /// once and calls this per format.
    ///
    /// # Errors
    /// See [`Sqzer::run`].
    pub fn encode(&self, decoded: &Decoded) -> Result<Output> {
        self.encode_with(decoded, |_| {})
    }

    /// [`Sqzer::encode`] that reports each step to `observe`, for a caller
    /// showing live progress. The steps are [`Progress`].
    ///
    /// # Errors
    /// See [`Sqzer::run`].
    pub fn encode_with(
        &self,
        decoded: &Decoded,
        mut observe: impl FnMut(Progress),
    ) -> Result<Output> {
        let image = &decoded.image;
        let (format, content) = self.pick(decoded);
        let encoder = self.registry.encoder(format)?;
        let caps = encoder.caps();

        let (bytes, target, report) = match self.params.target {
            Target::Ssimulacra2(_) if !caps.lossy => {
                let params = EncodeParams {
                    target: Target::Lossless,
                    ..self.params.clone()
                };
                (encoder.encode(image, &params)?, Resolved::Lossless, None)
            }
            Target::Ssimulacra2(t) if self.fast => {
                let Some(seed) = seeds::seed(caps.format, caps.tier, t) else {
                    return Err(Error::InvalidParams(format!(
                        "fast mode needs a calibrated seed table, and {} ({}) has none; \
                         use an explicit quality instead",
                        caps.name, caps.tier
                    )));
                };
                let params = EncodeParams {
                    target: Target::Quality(seed.quality),
                    ..self.params.clone()
                };
                (
                    encoder.encode(image, &params)?,
                    Resolved::Quality(seed.quality),
                    None,
                )
            }
            Target::Ssimulacra2(t) => {
                let mut reference = Reference::new(image)?;
                let search = seeded_search(encoder, t);
                let max = search.max_encodes;
                let mut n = 0u8;
                let found = search.encode_with(
                    encoder,
                    image,
                    &self.params,
                    &self.registry,
                    |candidate| reference.score(candidate),
                    |trial| {
                        n = n.saturating_add(1);
                        observe(Progress::Trial {
                            n,
                            max,
                            quality: trial.quality,
                            score: trial.score,
                        });
                    },
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
            backend: caps.name,
            tier: caps.tier,
            input: decoded.info,
            content,
            width: image.width(),
            height: image.height(),
            target,
            report,
        })
    }
}

impl Sqzer {
    /// The format [`Sqzer::encode`] would write for `decoded`: the one
    /// set with [`Sqzer::format`], else the content-aware default. Costs a
    /// pass over a sample of the pixels and no encode, so a dry run can
    /// name its outputs.
    #[must_use]
    pub fn pick_format(&self, decoded: &Decoded) -> Format {
        self.pick(decoded).0
    }

    fn pick(&self, decoded: &Decoded) -> (Format, Content) {
        let content = content::classify(&decoded.image);
        let format = self.format.unwrap_or_else(|| {
            default_format(&decoded.image, content, &self.params.target, &self.registry)
        });
        (format, content)
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

/// Output format when the caller names none, ADR-0001 D5.
///
/// A lossless target keeps PNG, which every build writes. Otherwise the
/// content decides: a graphic (few colours or large flat areas, see
/// [`sqzer_core::content`]) goes to lossless WebP when this build writes
/// it and to PNG when not, because a lossy codec gains little on such
/// input. A photograph goes to AVIF when this build has an encoder for it
/// and, for a perceptual target, a decoder to score its output with; else
/// PNG for transparent input and JPEG for the rest. Animation is not
/// modelled on [`Image`] yet, so animated input is treated as its first
/// frame.
fn default_format(img: &Image, content: Content, target: &Target, registry: &Registry) -> Format {
    if matches!(target, Target::Lossless) {
        return Format::Png;
    }
    if content == Content::Graphic {
        return if registry.has_encoder(Format::WebP) {
            Format::WebP
        } else {
            Format::Png
        };
    }
    let avif = registry.has_encoder(Format::Avif)
        && (!matches!(target, Target::Ssimulacra2(_)) || registry.has_decoder(Format::Avif));
    if avif {
        Format::Avif
    } else if img.has_alpha() {
        Format::Png
    } else {
        Format::Jpeg
    }
}

#[cfg(all(test, feature = "portable"))]
// Synthetic pixel data: the truncating casts are the point.
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use sqzer_core::image::ColorType;

    /// A flat 4 x 4 block: a graphic by the content heuristic.
    fn flat(color: ColorType) -> Image {
        Image::from_u8(4, 4, color, vec![200; 16 * color.channels()]).unwrap()
    }

    /// A 64 x 64 two-axis gradient: a photograph by the content heuristic.
    fn photo(color: ColorType) -> Image {
        let ch = color.channels();
        let mut samples = Vec::with_capacity(64 * 64 * ch);
        for y in 0..64u32 {
            for x in 0..64u32 {
                // Alpha varies so a PNG optimiser cannot drop the channel.
                let px = [
                    (x * 4) as u8,
                    (y * 4) as u8,
                    ((x + y) * 2) as u8,
                    64 + (x * 3) as u8,
                ];
                samples.extend_from_slice(&px[..ch]);
            }
        }
        Image::from_u8(64, 64, color, samples).unwrap()
    }

    fn png_bytes(color: ColorType) -> Vec<u8> {
        encode_png(&photo(color))
    }

    /// The portable registry, whatever features the build has: these
    /// tests are about the facade's decisions, which a native backend
    /// taking a format over would otherwise change from underneath.
    fn portable() -> Sqzer {
        let mut reg = Registry::new();
        sqzer_codecs::register_portable(&mut reg);
        Sqzer::with_registry(reg)
    }

    fn encode_png(img: &Image) -> Vec<u8> {
        sqzer_codecs::registry()
            .encoder(Format::Png)
            .unwrap()
            .encode(
                img,
                &EncodeParams {
                    target: Target::Lossless,
                    ..Default::default()
                },
            )
            .unwrap()
    }

    #[test]
    fn graphics_default_to_lossless_webp() {
        let out = portable().run(&encode_png(&flat(ColorType::Rgb))).unwrap();
        assert_eq!(out.content, Content::Graphic);
        assert_eq!(out.format, Format::WebP);
        assert_eq!(out.target, Resolved::Lossless);
        assert_eq!(out.backend, "image-webp");
        assert_eq!(out.tier, Tier::Portable);
        assert!(out.report.is_none());
        // Without a WebP encoder the graphic goes to PNG.
        let mut narrow = Registry::new();
        narrow.register_decoder(sqzer_codecs::png::PngDecoder);
        narrow.register_encoder(sqzer_codecs::png::PngEncoder);
        narrow.register_encoder(sqzer_codecs::avif::RavifEncoder);
        let out = Sqzer::with_registry(narrow)
            .target(Target::Quality(80.0))
            .run(&encode_png(&flat(ColorType::Rgb)))
            .unwrap();
        assert_eq!(out.format, Format::Png);
    }

    #[test]
    fn fast_mode_encodes_once_at_the_seed() {
        let out = portable()
            .fast(true)
            .format(Format::Jpeg)
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert!(out.report.is_none());
        let seed = seeds::seed(Format::Jpeg, Tier::Portable, 70.0).unwrap();
        assert_eq!(out.target, Resolved::Quality(seed.quality));
        // A lossless-only encoder needs no seed.
        let out = portable()
            .fast(true)
            .format(Format::Png)
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert_eq!(out.target, Resolved::Lossless);
    }

    #[test]
    fn fast_mode_without_a_seed_table_is_refused() {
        struct Unseeded;
        static CAPS: sqzer_core::codec::EncoderCaps = sqzer_core::codec::EncoderCaps {
            format: Format::Gif,
            name: "unseeded",
            lossy: true,
            lossless: false,
            alpha: false,
            animation: false,
            bit_depth: &[8],
            hdr: false,
            quality_range: 0.0..=100.0,
            effort_range: 0..=0,
            tier: Tier::Portable,
            options: &[],
        };
        impl Encoder for Unseeded {
            fn caps(&self) -> &sqzer_core::codec::EncoderCaps {
                &CAPS
            }
            fn encode(&self, _: &Image, _: &EncodeParams) -> Result<Vec<u8>> {
                Ok(vec![])
            }
        }
        let mut reg = Registry::new();
        reg.register_decoder(sqzer_codecs::png::PngDecoder);
        reg.register_encoder(Unseeded);
        let err = Sqzer::with_registry(reg)
            .fast(true)
            .format(Format::Gif)
            .run(&png_bytes(ColorType::Rgb))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidParams(_)), "{err}");
        assert!(err.to_string().contains("seed table"), "{err}");
    }

    #[test]
    fn preset_sets_target_and_effort_and_flags_override() {
        let s = Sqzer::new().preset(Preset::Archive);
        assert_eq!(s.params().target, Target::Ssimulacra2(85.0));
        assert_eq!(s.params().effort, 8);
        let s = s.target(Target::Quality(50.0)).effort(2);
        assert_eq!(s.params().target, Target::Quality(50.0));
        assert_eq!(s.params().effort, 2);
        let s = Sqzer::new()
            .preset(Preset::Lossless)
            .keep_icc(true)
            .auto_orient(false);
        assert_eq!(s.params().target, Target::Lossless);
        assert!(s.params().keep_icc);
        assert!(!s.decode_opts().apply_orientation);
        assert_eq!(s.format_choice(), None);
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn max_width_scales_down_and_the_search_scores_the_resized_image() {
        let s = portable().format(Format::Jpeg).max_width(32);
        let out = s.run(&png_bytes(ColorType::Rgb)).unwrap();
        assert_eq!((out.width, out.height), (32, 32));
        let report = out.report.expect("a perceptual target reports");
        // Scored against 32 x 32: against the 64 x 64 source the metric
        // would have refused the size mismatch.
        assert!(report.reached, "{report:?}");
        let back = s.decode(&out.bytes).unwrap();
        assert_eq!((back.image.width(), back.image.height()), (32, 32));
        // Both bounds: the tighter one decides.
        let out = portable()
            .format(Format::Png)
            .max_width(32)
            .max_height(8)
            .run(&png_bytes(ColorType::Rgba))
            .unwrap();
        assert_eq!((out.width, out.height), (8, 8));
    }

    #[test]
    fn resize_never_enlarges() {
        let s = portable()
            .format(Format::Png)
            .max_width(1600)
            .max_height(1600);
        let decoded = s.decode(&png_bytes(ColorType::Rgb)).unwrap();
        let same = s.transform(decoded.clone()).unwrap();
        assert_eq!(same, decoded);
        let out = s.encode(&same).unwrap();
        assert_eq!((out.width, out.height), (64, 64));
    }

    #[test]
    fn orientation_is_applied_before_the_resize() {
        // Stored 32 x 48 with EXIF orientation 6, displayed 48 x 32.
        let bytes = fixture("pattern-rot90.jpg");
        let s = portable().max_width(24);
        let upright = s.transform(s.decode(&bytes).unwrap()).unwrap().image;
        assert_eq!((upright.width(), upright.height()), (24, 16));
        // The bound is on the displayed width, and the pixels agree: the
        // same picture decoded upright and resized the same way.
        let plain = s
            .transform(s.decode(&fixture("pattern-rgb.jpg")).unwrap())
            .unwrap()
            .image;
        assert_eq!((plain.width(), plain.height()), (24, 16));
        let (a, b) = (
            upright.samples().as_u8().unwrap(),
            plain.samples().as_u8().unwrap(),
        );
        let worst = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(worst <= 40, "two JPEGs of one pattern differ by {worst}");
        // Without orientation the stored shape is what gets bounded.
        let s = s.auto_orient(false);
        let stored = s.transform(s.decode(&bytes).unwrap()).unwrap().image;
        assert_eq!((stored.width(), stored.height()), (24, 36));
    }

    #[test]
    fn thumbnail_preset_resizes_and_flags_override_it() {
        let s = Sqzer::new().preset(Preset::Thumbnail);
        assert_eq!(s.resize_bounds(), Preset::Thumbnail.resize());
        assert_eq!(s.resize_bounds().max_width, Some(512));
        // A later preset without a resize clears it.
        assert_eq!(s.clone().preset(Preset::Web).resize_bounds(), Resize::NONE);
        assert_eq!(s.clone().resize(Resize::NONE).resize_bounds(), Resize::NONE);
        let s = s.max_width(100);
        assert_eq!(s.resize_bounds().max_width, Some(100));
        assert_eq!(s.resize_bounds().max_height, Some(512));
    }

    #[test]
    fn a_zero_bound_is_refused() {
        let s = portable().max_height(0);
        let decoded = s.decode(&png_bytes(ColorType::Rgb)).unwrap();
        let err = s.transform(decoded).unwrap_err();
        assert!(matches!(err, Error::InvalidParams(_)), "{err}");
    }

    #[test]
    fn progress_reports_the_trials_the_report_lists() {
        let s = Sqzer::new().format(Format::Jpeg);
        let decoded = s.decode(&png_bytes(ColorType::Rgb)).unwrap();
        let mut seen = Vec::new();
        let out = s
            .encode_with(&decoded, |p| match p {
                Progress::Trial {
                    n, max, quality, ..
                } => {
                    assert_eq!(max, 6);
                    seen.push((n, quality));
                }
            })
            .unwrap();
        let report = out.report.unwrap();
        let expected: Vec<(u8, f32)> = report
            .trials
            .iter()
            .enumerate()
            .map(|(i, t)| (u8::try_from(i + 1).unwrap(), t.quality))
            .collect();
        assert_eq!(seen, expected);
        // No search, no progress.
        let mut count = 0;
        Sqzer::new()
            .format(Format::Png)
            .encode_with(&decoded, |_| count += 1)
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn decode_once_encode_many() {
        let s = Sqzer::new().target(Target::Quality(80.0));
        let decoded = s.decode(&png_bytes(ColorType::Rgb)).unwrap();
        assert_eq!(s.pick_format(&decoded), Format::Avif);
        let a = s.clone().format(Format::Jpeg).encode(&decoded).unwrap();
        let b = s.format(Format::Png).encode(&decoded).unwrap();
        assert_eq!(a.format, Format::Jpeg);
        assert_eq!(b.format, Format::Png);
        assert_eq!(a.input.format, Format::Png);
        assert_eq!((a.width, a.height), (64, 64));
    }

    #[test]
    fn perceptual_default_runs_the_search() {
        let out = portable().run(&png_bytes(ColorType::Rgb)).unwrap();
        assert_eq!(out.content, Content::Photo);
        assert_eq!(out.format, Format::Avif);
        assert_eq!(out.backend, "ravif");
        let report = out.report.expect("a perceptual target reports");
        assert!(
            report.iterations >= 1 && report.iterations <= 6,
            "{report:?}"
        );
        assert!(report.reached, "a smooth gradient is reachable: {report:?}");
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
        // Explicitly asked for, on a photograph.
        let out = portable()
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
        assert_eq!((out.width, out.height), (64, 64));
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
        let err = portable()
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
