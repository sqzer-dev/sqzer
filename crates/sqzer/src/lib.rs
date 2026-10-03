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
//! [`Sqzer::encode`]. [`Sqzer::run`] is the three in order. The transform
//! is [`Sqzer::prepare`] then [`Prepared::resize`], each returning the
//! value the next stage takes (ADR-0010), which a caller wanting several
//! sizes from one decode runs itself:
//!
//! ```no_run
//! use sqzer::Sqzer;
//! use sqzer::core::params::Resize;
//!
//! // One decode and one colour conversion, three widths.
//! let s = Sqzer::new();
//! let prepared = s.prepare(s.decode(&std::fs::read("photo.jpg").unwrap()).unwrap()).unwrap();
//! for width in [480, 960, 1600] {
//!     let ready = prepared.resize(&Resize::inside(Some(width), None)).unwrap();
//!     let out = s.encode(&ready).unwrap();
//!     std::fs::write(format!("photo-{}w.avif", out.width), &out.bytes).unwrap();
//! }
//! ```

pub use sqzer_codecs as codecs;
pub use sqzer_core as core;
pub use sqzer_metrics as metrics;

mod color;
mod resize;
mod stage;

pub use stage::{Prepared, Ready};

use std::sync::Arc;

use sqzer_core::codec::{Encoder, Format, FormatInfo, Tier};
use sqzer_core::content::{self, Content};
use sqzer_core::image::SampleFormat;
use sqzer_core::params::{DecodeOpts, EncodeParams, Preset, Resize, Resolved, Subsampling, Target};
use sqzer_core::resize::{Fit, Size};
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
    /// The bytes hold exactly the samples the encoder was given: the
    /// target resolved to lossless and the encoder is
    /// [exact](Encoder::exact) under its options. `false` under
    /// `png:colors`, which reduces the image to a palette at a lossless
    /// target. A promise read from the parameters, not a measurement: it
    /// is `false` under such an option even when the image lost nothing.
    pub lossless: bool,
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

    /// The resize as currently configured.
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

    /// Start from a preset: its target, effort and resize replace
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
    /// inside both. Sets the fit to [`Fit::Inside`] and turns enlarging
    /// off; a factor set before is replaced by the box.
    #[must_use]
    pub fn max_width(self, pixels: u32) -> Self {
        let height = self.box_side(false);
        self.bound(Some(pixels), height)
    }

    /// Scale down to at most this many pixels tall, keeping the aspect
    /// ratio. Never enlarges. See [`Sqzer::max_width`].
    #[must_use]
    pub fn max_height(self, pixels: u32) -> Self {
        let width = self.box_side(true);
        self.bound(width, Some(pixels))
    }

    fn box_side(&self, width: bool) -> Option<u32> {
        match self.resize.size {
            Size::Box {
                width: w,
                height: h,
            } => {
                if width {
                    w
                } else {
                    h
                }
            }
            Size::Scale(_) => None,
        }
    }

    fn bound(mut self, width: Option<u32>, height: Option<u32>) -> Self {
        self.resize.size = Size::Box { width, height };
        self.resize.fit = Fit::Inside;
        self.resize.enlarge = false;
        self
    }

    /// The whole resize request, replacing the current one: the box or
    /// factor, the fit, the position, the padding, enlarging and the
    /// filter (ADR-0009). [`Resize::NONE`] turns the stage off, for
    /// example after a preset that set it.
    #[must_use]
    pub fn resize(mut self, resize: Resize) -> Self {
        self.resize = resize;
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
    /// Samples and profile bytes then pass through untouched, and an
    /// encoder that cannot embed a profile refuses the image.
    #[must_use]
    pub fn keep_icc(mut self, keep: bool) -> Self {
        self.params.keep_icc = keep;
        self
    }

    /// Keep EXIF and XMP on the output instead of stripping them. EXIF
    /// orientation is applied and its tag reset either way. An encoder
    /// that cannot embed a blob refuses the image.
    #[must_use]
    pub fn keep_metadata(mut self, keep: bool) -> Self {
        self.params.keep_metadata = keep;
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
    /// [`Sqzer::encode`].
    ///
    /// A perceptual target runs the SSIMULACRA2 search of `sqzer-metrics`
    /// over the chosen encoder: up to six encodes, each decoded and scored
    /// against the transformed input, starting from the calibrated seed in
    /// [`sqzer_metrics::seeds`] when the backend has one. A target the
    /// encoder cannot reach is not an error; the best candidate is
    /// returned and [`Output::report`] says the target was missed. An
    /// encoder that only writes lossless meets any target with its one
    /// mode and skips the search. Whether that one encode kept every
    /// sample is [`Output::lossless`]: an option such as `png:colors`
    /// gives samples up there, and is refused next to an explicit
    /// [`Target::Lossless`] (ADR-0012 D4).
    ///
    /// > **Note**: scoring needs the output format decodable in this
    /// > build. A registry with an encoder and no decoder for a format
    /// > returns [`sqzer_core::Error::Unsupported`] for a perceptual
    /// > target there, and the default format steps around it.
    ///
    /// # Errors
    /// Unknown input, input this build recognises but cannot decode
    /// ([`sqzer_core::Error::DecoderUnavailable`], naming the feature or
    /// the missing library), an image over the pixel limit, a decoder
    /// failure, a resize [`Resize::check`] refuses or whose output is over
    /// the pixel limit, [`sqzer_core::Error::EncoderUnavailable`]
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

    /// The stages between decode and encode, ADR-0001 D3:
    /// [`Sqzer::prepare`], then [`Prepared::resize`] with the resize set on
    /// this builder, without copying an image the resize leaves alone. The
    /// result is what the encoder sees and what a perceptual target is
    /// scored against, once [`Sqzer::encode`] has added the padding a
    /// contain fit still owes.
    ///
    /// # Errors
    /// Those of [`Sqzer::prepare`] and [`Prepared::resize`].
    pub fn transform(&self, decoded: Decoded) -> Result<Ready> {
        self.prepare(decoded)?.into_ready(&self.resize)
    }

    /// The stages that run once per input, ADR-0007 D2, in this order. The
    /// result is resized with [`Prepared::resize`], once per size.
    ///
    /// 1. Colour (ADR-0007): an image carrying an ICC profile is converted
    ///    to sRGB and the profile dropped, unless [`Sqzer::keep_icc`] is
    ///    set, in which case samples and profile pass through untouched. A
    ///    profile that does not fit the image's layout is dropped without
    ///    a conversion; one that cannot be parsed is an error.
    /// 2. Range: float samples, which are linear light, are encoded with
    ///    the sRGB curve into 16 bits, clipped at display white. No
    ///    encoder in this build takes float input, and the metric then
    ///    scores what the encoder gets. See
    ///    [`Image::to_u16`](sqzer_core::image::Image::to_u16). A float
    ///    image that keeps its ICC profile under [`Sqzer::keep_icc`] is
    ///    refused: the curve would leave the profile describing samples
    ///    it no longer matches.
    /// 3. Metadata (ADR-0001 D7): EXIF and XMP are dropped unless
    ///    [`Sqzer::keep_metadata`] is set. Orientation was applied by the
    ///    decoder and the EXIF tag reset there, so kept EXIF never
    ///    contradicts the pixels.
    ///
    /// # Errors
    /// [`sqzer_core::Error::Transform`] for a profile that cannot be
    /// parsed, a LUT profile on float samples, or a kept profile on float
    /// samples.
    pub fn prepare(&self, decoded: Decoded) -> Result<Prepared> {
        let image = if self.params.keep_icc {
            decoded.image
        } else {
            color::to_srgb(decoded.image)?
        };
        if image.icc().is_some() && image.sample_format() == SampleFormat::F32 {
            return Err(Error::Transform {
                stage: "range",
                message: "float samples cannot keep their ICC profile: encoding them for an \
                          integer codec would leave the profile describing other values; drop \
                          keep_icc"
                    .into(),
            });
        }
        let mut image = image.to_u16().into_owned();
        if !self.params.keep_metadata {
            image.strip_metadata();
        }
        Ok(Prepared {
            image,
            info: decoded.info,
            max_pixels: self.decode.max_pixels,
        })
    }

    /// Encode a transformed image. A pending [`Ready::canvas`] is padded
    /// here, with the background the encoder calls for: the one the resize
    /// names, else transparent where the encoder takes alpha and white
    /// where it does not. Everything [`Sqzer::run`] says about targets and
    /// errors applies; a caller that wants several output formats from one
    /// input transforms once and calls this per format.
    ///
    /// # Errors
    /// See [`Sqzer::run`], and [`sqzer_core::Error::Unsupported`] for a
    /// translucent padding on an encoder without alpha (ADR-0010 D3).
    pub fn encode(&self, ready: &Ready) -> Result<Output> {
        self.encode_with(ready, |_| {})
    }

    /// [`Sqzer::encode`] that reports each step to `observe`, for a caller
    /// showing live progress. The steps are [`Progress`].
    ///
    /// # Errors
    /// See [`Sqzer::run`].
    pub fn encode_with(&self, ready: &Ready, mut observe: impl FnMut(Progress)) -> Result<Output> {
        let (format, content) = self.pick(ready);
        let encoder = self.registry.encoder(format)?;
        let caps = encoder.caps();
        if ready.translucent_padding() && !caps.alpha {
            return Err(Error::Unsupported {
                format,
                what: "a translucent background: it needs an alpha channel".into(),
            });
        }
        let padded;
        let image = match &ready.canvas {
            Some(canvas) => {
                padded = resize::pad(&ready.image, canvas, caps.alpha)?;
                &padded
            }
            None => &ready.image,
        };

        let lossy_options = self.lossy_options(encoder);
        if self.params.target == Target::Lossless && !lossy_options.is_empty() {
            return Err(Error::InvalidParams(format!(
                "a lossless target contradicts {}, which gives samples up; drop one of them",
                lossy_options.join(" and ")
            )));
        }

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
            input: ready.info,
            content,
            width: image.width(),
            height: image.height(),
            target,
            lossless: target == Resolved::Lossless && lossy_options.is_empty(),
            report,
        })
    }

    /// The codec options set on this builder under which `encoder` gives
    /// samples up at a lossless target, each as `` `codec:key=value` ``.
    /// Empty when, and only when, the encoder is [exact](Encoder::exact)
    /// under all of them together.
    ///
    /// [`Sqzer::encode`] refuses a lossless target when this is not empty
    /// (ADR-0012 D4); a caller that wants to refuse earlier asks here.
    #[must_use]
    pub fn lossy_options(&self, encoder: &dyn Encoder) -> Vec<String> {
        if encoder.exact(&self.params) {
            return Vec::new();
        }
        // Each option on its own, to name the ones that matter.
        let name = |(key, value): (&String, &String)| format!("`{key}={value}`");
        let options = &self.params.codec_specific;
        let named: Vec<String> = options
            .iter()
            .filter(|&(key, value)| {
                let mut alone = EncodeParams {
                    codec_specific: std::collections::BTreeMap::new(),
                    ..self.params.clone()
                };
                alone.codec_specific.insert(key.clone(), value.clone());
                !encoder.exact(&alone)
            })
            .map(name)
            .collect();
        if named.is_empty() {
            // Lossy only in combination: no single option is to blame,
            // and the answer must still not be "none".
            return options.iter().map(name).collect();
        }
        named
    }
}

impl Sqzer {
    /// The format [`Sqzer::encode`] would write for `ready`: the one set
    /// with [`Sqzer::format`], else the content-aware default. Costs a
    /// pass over a sample of the pixels and no encode, so a dry run can
    /// name its outputs.
    #[must_use]
    pub fn pick_format(&self, ready: &Ready) -> Format {
        self.pick(ready).0
    }

    fn pick(&self, ready: &Ready) -> (Format, Content) {
        let content = content::classify(&ready.image);
        // Translucent padding is alpha the output must carry (ADR-0010 D3).
        let alpha = ready.image.has_alpha() || ready.translucent_padding();
        let format = self
            .format
            .unwrap_or_else(|| default_format(alpha, content, &self.params.target, &self.registry));
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
/// PNG for transparent input, `alpha`, and JPEG for the rest. Animation
/// is not modelled on [`sqzer_core::image::Image`] yet, so animated input is treated as its
/// first frame.
fn default_format(alpha: bool, content: Content, target: &Target, registry: &Registry) -> Format {
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
    } else if alpha {
        Format::Png
    } else {
        Format::Jpeg
    }
}

#[cfg(all(test, feature = "portable"))]
// Synthetic pixel data: the truncating casts are the point, and the
// averages over it are small.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
mod tests {
    use super::*;
    use sqzer_core::image::{ColorType, Image};

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
        narrow.register_encoder(sqzer_codecs::oxipng::OxipngEncoder);
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
            exif: false,
            xmp: false,
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
        assert_eq!(same.image(), &decoded.image);
        assert_eq!(same.canvas(), None);
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
        assert_eq!(s.resize_bounds(), Resize::inside(Some(512), Some(512)));
        // A later preset without a resize clears it.
        assert_eq!(s.clone().preset(Preset::Web).resize_bounds(), Resize::NONE);
        assert_eq!(s.clone().resize(Resize::NONE).resize_bounds(), Resize::NONE);
        let s = s.max_width(100);
        assert_eq!(s.resize_bounds(), Resize::inside(Some(100), Some(512)));
        // A bound turns a cover fit, enlarging or a factor back into a
        // plain bound.
        let cover = Resize {
            size: Size::Scale(2.0),
            fit: Fit::Cover,
            enlarge: true,
            ..Resize::NONE
        };
        let s = Sqzer::new().resize(cover).max_height(64);
        assert_eq!(s.resize_bounds(), Resize::inside(None, Some(64)));
    }

    fn contain(w: u32, h: u32) -> Resize {
        Resize {
            size: Size::Box {
                width: Some(w),
                height: Some(h),
            },
            fit: Fit::Contain,
            ..Resize::NONE
        }
    }

    #[test]
    fn contain_returns_the_full_box_padded_per_encoder() {
        // 64 x 64 into 32 x 16: resampled to 16 x 16, padded to 32 x 16.
        let bytes = png_bytes(ColorType::Rgb);
        let png = portable().format(Format::Png).resize(contain(32, 16));
        let out = png.run(&bytes).unwrap();
        assert_eq!((out.width, out.height), (32, 16));
        // PNG takes alpha: the padding is transparent.
        let back = png.decode(&out.bytes).unwrap().image;
        assert_eq!(back.color(), ColorType::Rgba);
        assert_eq!(&back.samples().as_u8().unwrap()[..4], &[0, 0, 0, 0]);
        assert_eq!(back.samples().as_u8().unwrap()[16 * 4 + 3], 255);
        // JPEG does not: white, and the search scored the padded image.
        let jpeg = portable().format(Format::Jpeg).resize(contain(32, 16));
        let out = jpeg.run(&bytes).unwrap();
        assert_eq!((out.width, out.height), (32, 16));
        assert!(out.report.expect("searched").reached);
        let back = jpeg.decode(&out.bytes).unwrap().image;
        // White to JPEG noise: the corner MCU also holds image pixels.
        assert!(
            back.samples().as_u8().unwrap()[..3]
                .iter()
                .all(|&s| s > 235),
            "{:?}",
            &back.samples().as_u8().unwrap()[..3]
        );
        // A named background wins over both defaults.
        let red = Resize {
            background: Some([255, 0, 0, 255]),
            ..contain(32, 16)
        };
        let out = png.clone().resize(red).run(&bytes).unwrap();
        let back = png.decode(&out.bytes).unwrap().image;
        assert_eq!(back.color(), ColorType::Rgb);
        assert_eq!(&back.samples().as_u8().unwrap()[..3], &[255, 0, 0]);
        // The canvas is pending after the transform, and applied only by
        // the encode.
        let sized = png.transform(png.decode(&bytes).unwrap()).unwrap();
        assert_eq!((sized.image.width(), sized.image.height()), (16, 16));
        assert_eq!(sized.canvas.map(|c| (c.width, c.height)), Some((32, 16)));
    }

    #[test]
    fn a_translucent_background_needs_an_encoder_with_alpha() {
        let clear = Resize {
            background: Some([0, 0, 0, 0]),
            ..contain(32, 16)
        };
        let bytes = png_bytes(ColorType::Rgb);
        let err = portable()
            .format(Format::Jpeg)
            .target(Target::Quality(80.0))
            .resize(clear)
            .run(&bytes)
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::Unsupported {
                    format: Format::Jpeg,
                    ..
                }
            ),
            "{err}"
        );
        // The default format counts it as alpha: without AVIF, an opaque
        // photo on a clear canvas goes to PNG, not JPEG.
        let mut narrow = Registry::new();
        narrow.register_decoder(sqzer_codecs::png::PngDecoder);
        narrow.register_encoder(sqzer_codecs::oxipng::OxipngEncoder);
        narrow.register_encoder(sqzer_codecs::jpeg::MozjpegEncoder);
        let s = Sqzer::with_registry(narrow).target(Target::Quality(80.0));
        assert_eq!(s.run(&bytes).unwrap().format, Format::Jpeg);
        assert_eq!(s.resize(clear).run(&bytes).unwrap().format, Format::Png);
    }

    #[test]
    fn one_prepared_image_serves_several_sizes() {
        let s = portable().format(Format::Png);
        let prepared = s
            .prepare(s.decode(&png_bytes(ColorType::Rgb)).unwrap())
            .unwrap();
        let sizes: Vec<(u32, u32)> = [48, 16, 32, 100]
            .into_iter()
            .map(|w| {
                let d = prepared.resize(&Resize::inside(Some(w), None)).unwrap();
                (d.image.width(), d.image.height())
            })
            .collect();
        assert_eq!(sizes, [(48, 48), (16, 16), (32, 32), (64, 64)]);
        // Each size starts from the source: 32 after 16 is as sharp as 32
        // alone.
        let alone = prepared.resize(&Resize::inside(Some(32), None)).unwrap();
        let direct = s
            .clone()
            .max_width(32)
            .transform(s.decode(&png_bytes(ColorType::Rgb)).unwrap())
            .unwrap();
        assert_eq!(alone, direct);
    }

    #[test]
    fn cover_crops_to_the_box() {
        let cover = Resize {
            size: Size::Box {
                width: Some(40),
                height: Some(20),
            },
            fit: Fit::Cover,
            ..Resize::NONE
        };
        let out = portable()
            .format(Format::Png)
            .resize(cover)
            .run(&png_bytes(ColorType::Rgba))
            .unwrap();
        assert_eq!((out.width, out.height), (40, 20));
    }

    #[test]
    fn an_enlarge_past_the_pixel_limit_is_refused() {
        let s = portable().max_pixels(10_000).resize(Resize {
            size: Size::Scale(4.0),
            enlarge: true,
            ..Resize::NONE
        });
        let err = s
            .transform(s.decode(&png_bytes(ColorType::Rgb)).unwrap())
            .unwrap_err();
        assert!(matches!(err, Error::InvalidParams(_)), "{err}");
        assert!(err.to_string().contains("256x256"), "{err}");
    }

    /// The P3-tagged pattern in every container that carries one, decoded
    /// and converted, against the untagged pattern in the same container.
    #[test]
    fn a_p3_source_is_converted_to_srgb_and_loses_its_profile() {
        for (tagged, plain) in [
            ("pattern-icc.jpg", "pattern-rgb.jpg"),
            ("pattern-icc.webp", "pattern-rgb.webp"),
            ("pattern-icc.jxl", "pattern-rgb.jxl"),
            ("pattern-icc.tif", "pattern-rgb.tif"),
        ] {
            let s = portable();
            let src = s.decode(&fixture(tagged)).unwrap();
            assert!(
                src.image.icc().is_some(),
                "{tagged} lost its profile in decode"
            );
            let out = s.transform(src.clone()).unwrap().image;
            assert_eq!(out.icc(), None, "{tagged}");
            let before = src.image.samples().as_u8().unwrap();
            let after = out.samples().as_u8().unwrap();
            // The tagged file holds the same samples as the untagged one
            // (to JPEG noise): the profile is the only difference.
            let plain = s.decode(&fixture(plain)).unwrap();
            let plain = plain.image.samples().as_u8().unwrap();
            let mae = plain
                .iter()
                .zip(before)
                .map(|(a, b)| f64::from(a.abs_diff(*b)))
                .sum::<f64>()
                / plain.len() as f64;
            assert!(
                mae <= 3.0,
                "{tagged}: mean difference {mae} to the untagged pattern"
            );
            // The pattern's ramps are saturated colours: in sRGB they read
            // more saturated than the same numbers did in P3, so the
            // spread between a pixel's channels grows on average.
            let spread = |v: &[u8]| -> f64 {
                let px = v.as_chunks::<3>().0;
                px.iter()
                    .map(|px| f64::from(px.iter().max().unwrap() - px.iter().min().unwrap()))
                    .sum::<f64>()
                    / px.len() as f64
            };
            assert!(
                spread(after) > spread(before) + 4.0,
                "{tagged}: spread {} -> {}",
                spread(before),
                spread(after)
            );
            // Gray-balanced pixels stay put: same white, same curve.
            let is_neutral = |px: &[u8]| px[0].abs_diff(px[1]) <= 6 && px[1].abs_diff(px[2]) <= 6;
            let mut neutrals = 0;
            for (a, b) in before
                .as_chunks::<3>()
                .0
                .iter()
                .zip(after.as_chunks::<3>().0)
            {
                if is_neutral(a) {
                    neutrals += 1;
                    for c in 0..3 {
                        assert!(a[c].abs_diff(b[c]) <= 6, "{tagged}: neutral {a:?} -> {b:?}");
                    }
                }
            }
            assert!(neutrals > 0, "{tagged}: the pattern has neutral pixels");
        }
    }

    #[test]
    fn keep_icc_leaves_bytes_and_samples_alone() {
        let s = portable().keep_icc(true).format(Format::Png);
        let src = s.decode(&fixture("pattern-icc.webp")).unwrap();
        let out = s.run(&fixture("pattern-icc.webp")).unwrap();
        let back = s.decode(&out.bytes).unwrap().image;
        assert_eq!(back.icc(), src.image.icc());
        assert_eq!(back.samples(), src.image.samples());
        // And without it the PNG is untagged with converted samples.
        let out = portable()
            .format(Format::Png)
            .run(&fixture("pattern-icc.webp"))
            .unwrap();
        let back = s.decode(&out.bytes).unwrap().image;
        assert_eq!(back.icc(), None);
        assert_ne!(back.samples(), src.image.samples());
    }

    #[test]
    fn metadata_is_stripped_unless_kept_and_orientation_is_reset_either_way() {
        use sqzer_core::image::reset_exif_orientation;
        let s = portable().format(Format::Png);
        let src = s.decode(&fixture("pattern-meta.jpg")).unwrap().image;
        assert!(src.exif().is_some() && src.xmp().is_some());
        let out = s.run(&fixture("pattern-meta.jpg")).unwrap();
        assert_eq!((out.width, out.height), (48, 32));
        let back = s.decode(&out.bytes).unwrap().image;
        assert_eq!((back.exif(), back.xmp()), (None, None));

        let s = s.keep_metadata(true);
        let out = s.run(&fixture("pattern-meta.jpg")).unwrap();
        let back = s.decode(&out.bytes).unwrap().image;
        assert_eq!(back.exif(), src.exif());
        assert_eq!(back.xmp(), src.xmp());
        // The tag is already 1: resetting it changes nothing.
        let mut copy = back.exif().unwrap().to_vec();
        assert!(reset_exif_orientation(&mut copy));
        assert_eq!(copy.as_slice(), back.exif().unwrap());

        // Not oriented: pixels as stored, tag as stored.
        let s = s.auto_orient(false);
        let out = s.run(&fixture("pattern-meta.jpg")).unwrap();
        assert_eq!((out.width, out.height), (32, 48));
        let back = s.decode(&out.bytes).unwrap().image;
        let mut copy = back.exif().unwrap().to_vec();
        assert!(reset_exif_orientation(&mut copy));
        assert_ne!(copy.as_slice(), back.exif().unwrap());

        // An encoder that cannot carry a blob refuses rather than drops.
        let err = portable()
            .format(Format::Avif)
            .target(Target::Quality(50.0))
            .keep_metadata(true)
            .run(&fixture("pattern-meta.jpg"))
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
        assert!(err.to_string().contains("XMP"), "{err}");
    }

    #[test]
    fn openexr_goes_through_every_encoder_as_sixteen_bit_srgb() {
        use sqzer_core::image::SampleFormat;
        let s = portable();
        let decoded = s.decode(&fixture("pattern-rgb.exr")).unwrap();
        assert_eq!(decoded.image.sample_format(), SampleFormat::F32);
        let ready = s.transform(decoded).unwrap();
        assert_eq!(ready.image.sample_format(), SampleFormat::U16);
        // The 16-bit picture is the pattern: the fixture holds it in linear
        // light and the range stage encodes it back.
        let pattern = s.decode(&fixture("pattern-rgb.webp")).unwrap().image;
        let worst = ready
            .image
            .to_u8(Format::Png)
            .unwrap()
            .samples()
            .as_u8()
            .unwrap()
            .iter()
            .zip(pattern.samples().as_u8().unwrap())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 1, "worst step {worst}");
        // Every encoder in the build takes it, searched or not.
        for format in [Format::Jpeg, Format::Png, Format::WebP, Format::Avif] {
            let out = s
                .clone()
                .format(format)
                .run(&fixture("pattern-rgb.exr"))
                .unwrap_or_else(|e| panic!("{format}: {e}"));
            assert_eq!(out.input.format, Format::Exr);
            assert_eq!((out.width, out.height), (48, 32));
        }
        let out = s
            .format(Format::Jpeg)
            .run(&fixture("pattern-rgb.exr"))
            .unwrap();
        assert!(out.report.expect("searched").reached);
    }

    #[test]
    fn a_float_image_cannot_keep_its_profile() {
        use sqzer_core::image::{ColorType, Samples};
        let img = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3]))
            .unwrap()
            .with_icc(Some(
                moxcms::ColorProfile::new_display_p3().encode().unwrap(),
            ));
        let decoded = Decoded {
            image: img,
            info: FormatInfo {
                format: Format::Exr,
                animated: false,
            },
        };
        let err = portable()
            .keep_icc(true)
            .transform(decoded.clone())
            .unwrap_err();
        assert!(
            matches!(err, Error::Transform { stage: "range", .. }),
            "{err}"
        );
        // Without keep_icc the primaries are applied and the profile goes.
        let out = portable().transform(decoded).unwrap().image;
        assert_eq!(out.icc(), None);
        assert_eq!(out.sample_format(), SampleFormat::U16);
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
        let decoded = s
            .transform(s.decode(&png_bytes(ColorType::Rgb)).unwrap())
            .unwrap();
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
        let decoded = s
            .transform(s.decode(&png_bytes(ColorType::Rgb)).unwrap())
            .unwrap();
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
    fn a_palette_is_one_lossy_encode_under_the_default_target() {
        let input = png_bytes(ColorType::Rgba);
        let plain = portable().format(Format::Png).run(&input).unwrap();
        assert_eq!(plain.target, Resolved::Lossless);
        assert!(plain.lossless);

        let out = portable()
            .format(Format::Png)
            .codec_opt("png", "colors", "16")
            .run(&input)
            .unwrap();
        // Still the encoder's one mode, with no search and no score, and
        // no longer a promise of exact samples.
        assert_eq!(out.target, Resolved::Lossless);
        assert!(out.report.is_none());
        assert!(!out.lossless);
        // No claim on size: a dithered palette of a smooth gradient is
        // harder on PNG's filters than the gradient was.
        let back = Sqzer::new().decode(&out.bytes).unwrap();
        let colors: std::collections::HashSet<_> = back
            .image
            .samples()
            .as_u8()
            .unwrap()
            .chunks_exact(back.image.channels())
            .collect();
        assert!(colors.len() <= 16, "{} colours", colors.len());
    }

    #[test]
    fn a_lossless_target_refuses_an_option_that_gives_samples_up() {
        let input = png_bytes(ColorType::Rgba);
        let lossless = || portable().format(Format::Png).target(Target::Lossless);
        for (key, value) in [("colors", "16"), ("optimize_alpha", "true")] {
            let err = lossless()
                .codec_opt("png", key, value)
                .run(&input)
                .unwrap_err();
            let named = format!("`png:{key}={value}`");
            assert!(
                matches!(&err, Error::InvalidParams(m) if m.contains(&named)),
                "{err}"
            );
            // Without the explicit target the option is honoured and
            // the output says what it is.
            let out = portable()
                .format(Format::Png)
                .codec_opt("png", key, value)
                .run(&input)
                .unwrap();
            assert!(!out.lossless, "png:{key}={value}");
        }
        // Only the options that matter are named.
        let err = lossless()
            .codec_opt("png", "interlace", "true")
            .codec_opt("png", "dither", "50")
            .codec_opt("png", "colors", "16")
            .run(&input)
            .unwrap_err();
        let Error::InvalidParams(message) = &err else {
            panic!("{err}");
        };
        assert!(message.contains("`png:colors=16`"), "{message}");
        assert!(
            !message.contains("interlace") && !message.contains("dither"),
            "{message}"
        );

        // Options that keep every sample, and the defaults spelled out,
        // pass and stay lossless.
        let out = lossless()
            .codec_opt("png", "interlace", "true")
            .codec_opt("png", "optimize_alpha", "false")
            .codec_opt("png", "colors", "off")
            .run(&input)
            .unwrap();
        assert!(out.lossless);
        // An option addressed to another encoder is not this one's.
        let out = portable()
            .format(Format::WebP)
            .target(Target::Lossless)
            .codec_opt("png", "colors", "16")
            .run(&input)
            .unwrap();
        assert!(out.lossless);
    }

    #[test]
    fn options_that_are_lossy_only_together_are_still_lossy() {
        // Exact under either option alone, not under both.
        struct Pair;
        static CAPS: sqzer_core::codec::EncoderCaps = sqzer_core::codec::EncoderCaps {
            format: Format::Gif,
            name: "pair",
            lossy: false,
            lossless: true,
            alpha: false,
            animation: false,
            bit_depth: &[8],
            hdr: false,
            exif: false,
            xmp: false,
            quality_range: 100.0..=100.0,
            effort_range: 0..=0,
            tier: Tier::Portable,
            options: &[],
        };
        impl Encoder for Pair {
            fn caps(&self) -> &sqzer_core::codec::EncoderCaps {
                &CAPS
            }
            fn encode(&self, _: &Image, _: &EncodeParams) -> Result<Vec<u8>> {
                Ok(vec![])
            }
            fn exact(&self, params: &EncodeParams) -> bool {
                params.codec_opts("gif").count() < 2
            }
        }
        let sqzer = || {
            let mut reg = Registry::new();
            reg.register_decoder(sqzer_codecs::png::PngDecoder);
            reg.register_encoder(Pair);
            Sqzer::with_registry(reg).format(Format::Gif)
        };
        let input = png_bytes(ColorType::Rgb);

        let one = sqzer().codec_opt("gif", "a", "1");
        assert_eq!(one.lossy_options(&Pair), [] as [String; 0]);
        assert!(one.target(Target::Lossless).run(&input).unwrap().lossless);

        let both = sqzer()
            .codec_opt("gif", "a", "1")
            .codec_opt("gif", "b", "2");
        assert_eq!(both.lossy_options(&Pair), ["`gif:a=1`", "`gif:b=2`"]);
        assert!(!both.run(&input).unwrap().lossless);
        let err = both.target(Target::Lossless).run(&input).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidParams(m) if m.contains("`gif:a=1` and `gif:b=2`")),
            "{err}"
        );
    }

    #[test]
    fn explicit_quality_and_lossless_do_not_report() {
        let out = Sqzer::new()
            .target(Target::Quality(80.0))
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert!(out.report.is_none());
        assert!(!out.lossless);
        let out = Sqzer::new()
            .target(Target::Lossless)
            .run(&png_bytes(ColorType::Rgb))
            .unwrap();
        assert!(out.report.is_none());
        assert!(out.lossless);
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

    /// An AVIF encoder with no AVIF decoder, as a narrowed registry can be.
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
        narrow.register_encoder(sqzer_codecs::oxipng::OxipngEncoder);
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
