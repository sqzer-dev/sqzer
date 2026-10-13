//! The options objects of ADR-0011 D3: the command line's flags in camel
//! case, turned into the same builder calls in the same order, with the
//! rules the command line enforces between them.

use std::collections::BTreeMap;

use js_sys::{Function, Object};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sqzer::Sqzer;
use sqzer::core::params::Target;
use sqzer::core::resize::{Resize, Size};
use sqzer::core::{Error, Result, codec, params, resize};
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

use crate::format_name;

/// The pixel limit of the package, against 268 megapixels in the library:
/// the target search reserves about 160 bytes per pixel, and this much of
/// that fits the 4 GiB a wasm32 module can address (ADR-0011 D4).
pub const MAX_PIXELS: u32 = 24_000_000;

/// What `optimize` and `SqzerImage.encode` take: the flags of the `sqzer`
/// command line in camel case. Every key is optional.
#[derive(Debug, Default, Deserialize, Tsify)]
#[serde(rename_all = "camelCase", default)]
pub struct Options {
    /// Output format. Left out, it is chosen per image: AVIF for
    /// photographs, lossless WebP for graphics.
    pub format: Option<Format>,
    /// SSIMULACRA2 score to reach by searching encoder quality. Default
    /// 70. 100 is identical, 50 shows artefacts on close inspection.
    pub target: Option<f32>,
    /// Encode at this abstract quality, 0 to 100, with no search.
    pub quality: Option<f32>,
    /// Lossless output. Refused by encoders that have no lossless mode.
    pub lossless: Option<bool>,
    /// Named settings, which `target`, `quality`, `lossless`, `effort` and
    /// the resize keys override.
    pub preset: Option<Preset>,
    /// Effort, 0 to 10. Higher is slower and smaller.
    pub effort: Option<u8>,
    /// Chroma subsampling, for codecs that have it.
    pub subsampling: Option<Subsampling>,
    /// Keep the ICC profile instead of converting to sRGB.
    pub keep_icc: Option<bool>,
    /// Keep EXIF and XMP instead of stripping them.
    pub keep_metadata: Option<bool>,
    /// Skip the search: encode once at the calibrated seed quality for the
    /// target.
    pub fast: Option<bool>,
    /// Target width in pixels. With the default `fit`, the image is scaled
    /// down to fit inside `width` x `height`; one side may be given alone.
    pub width: Option<u32>,
    /// Target height in pixels.
    pub height: Option<u32>,
    /// How the image meets the `width` x `height` box, as in CSS
    /// `object-fit`. Default `inside`; the others need both sides.
    pub fit: Option<Fit>,
    /// Where `cover` crops and `contain` places the image. Default
    /// `center`.
    pub position: Option<Position>,
    /// The padding of `contain`: `#rgb`, `#rrggbb`, `#rrggbbaa`, `white`,
    /// `black` or `transparent`. Default: transparent for a format that
    /// has alpha, white for one that does not.
    pub background: Option<String>,
    /// Scale by a factor instead of a box: `0.5` halves both sides. Above
    /// 1 needs `enlarge`.
    pub scale: Option<f32>,
    /// Allow scaling up. Without it no fit ever scales up.
    pub enlarge: Option<bool>,
    /// Resampling filter. Default `lanczos3`.
    pub filter: Option<Filter>,
    /// Refuse an image above this many pixels, decoded or resized.
    /// Default 24 million.
    pub max_pixels: Option<u32>,
    /// Backend-specific options, `{ "avif:bit_depth": "8" }`. `codecs()`
    /// lists every key with its default. Unknown keys are an error.
    pub codec_opts: BTreeMap<String, String>,
    /// Called once per trial of the target search, as it is scored.
    #[serde(with = "serde_wasm_bindgen::preserve")]
    #[tsify(type = "(trial: TrialProgress) => void")]
    pub on_trial: JsValue,
}

/// What `decode` takes.
#[derive(Debug, Default, Deserialize, Tsify)]
#[serde(rename_all = "camelCase", default)]
pub struct DecodeOptions {
    /// Refuse an image above this many pixels. Default 24 million.
    pub max_pixels: Option<u32>,
}

/// What `decodeAny` takes.
#[derive(Debug, Default, Deserialize, Tsify)]
#[serde(rename_all = "camelCase", default)]
pub struct DecodeAnyOptions {
    /// Refuse an image above this many pixels. Default 24 million.
    pub max_pixels: Option<u32>,
    /// The width a vector image is rasterised to fit inside. Other input
    /// is decoded at its own size; `encode` resizes it.
    pub width: Option<u32>,
    /// The height a vector image is rasterised to fit inside.
    pub height: Option<u32>,
}

/// An output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Tsify)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// JPEG.
    #[serde(alias = "jpg")]
    Jpeg,
    /// PNG.
    Png,
    /// WebP.
    Webp,
    /// AVIF.
    Avif,
    /// JPEG XL.
    Jxl,
}

impl From<Format> for codec::Format {
    fn from(f: Format) -> Self {
        match f {
            Format::Jpeg => Self::Jpeg,
            Format::Png => Self::Png,
            Format::Webp => Self::WebP,
            Format::Avif => Self::Avif,
            Format::Jxl => Self::Jxl,
        }
    }
}

/// `web` (target 70), `thumbnail` (60, fit inside 512 x 512), `archive`
/// (85) or `lossless`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Tsify)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    /// Target 70, effort 6.
    Web,
    /// Target 60, effort 6, fit inside 512 x 512.
    Thumbnail,
    /// Target 85, effort 8.
    Archive,
    /// Lossless, effort 8.
    Lossless,
}

impl From<Preset> for params::Preset {
    fn from(p: Preset) -> Self {
        match p {
            Preset::Web => Self::Web,
            Preset::Thumbnail => Self::Thumbnail,
            Preset::Archive => Self::Archive,
            Preset::Lossless => Self::Lossless,
        }
    }
}

/// Chroma subsampling. `auto` lets the backend decide from the quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Tsify)]
pub enum Subsampling {
    /// The backend decides from the quality.
    #[serde(rename = "auto")]
    Auto,
    /// No chroma subsampling.
    #[serde(rename = "444")]
    S444,
    /// Horizontal subsampling.
    #[serde(rename = "422")]
    S422,
    /// Horizontal and vertical subsampling.
    #[serde(rename = "420")]
    S420,
}

impl From<Subsampling> for params::Subsampling {
    fn from(s: Subsampling) -> Self {
        match s {
            Subsampling::Auto => Self::Auto,
            Subsampling::S444 => Self::S444,
            Subsampling::S422 => Self::S422,
            Subsampling::S420 => Self::S420,
        }
    }
}

/// How the image meets the box, as in CSS `object-fit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Tsify)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    /// Fit inside the box, keep the aspect ratio.
    Inside,
    /// Cover the box, crop the overflow.
    Cover,
    /// Fit inside the box, pad to it.
    Contain,
    /// Stretch to the box.
    Fill,
    /// Cover the box without cropping.
    Outside,
}

impl From<Fit> for resize::Fit {
    fn from(f: Fit) -> Self {
        match f {
            Fit::Inside => Self::Inside,
            Fit::Cover => Self::Cover,
            Fit::Contain => Self::Contain,
            Fit::Fill => Self::Fill,
            Fit::Outside => Self::Outside,
        }
    }
}

/// Where `cover` crops and `contain` places the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Tsify)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    /// Centred.
    Center,
    /// Top edge.
    Top,
    /// Bottom edge.
    Bottom,
    /// Left edge.
    Left,
    /// Right edge.
    Right,
    /// Top-left corner.
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
}

impl From<Position> for resize::Position {
    fn from(p: Position) -> Self {
        match p {
            Position::Center => Self::Center,
            Position::Top => Self::Top,
            Position::Bottom => Self::Bottom,
            Position::Left => Self::Left,
            Position::Right => Self::Right,
            Position::TopLeft => Self::TopLeft,
            Position::TopRight => Self::TopRight,
            Position::BottomLeft => Self::BottomLeft,
            Position::BottomRight => Self::BottomRight,
        }
    }
}

/// The resampling filter. `nearest` keeps the exact colours of pixel art.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Tsify)]
#[serde(rename_all = "kebab-case")]
pub enum Filter {
    /// Lanczos3, sharp with little ringing.
    Lanczos3,
    /// Mitchell-Netravali bicubic, softer.
    Mitchell,
    /// Catmull-Rom bicubic.
    CatmullRom,
    /// Bilinear.
    Bilinear,
    /// Box, every source pixel weighted equally.
    Box,
    /// Nearest neighbour, for pixel art.
    Nearest,
}

impl From<Filter> for resize::Filter {
    fn from(f: Filter) -> Self {
        match f {
            Filter::Lanczos3 => Self::Lanczos3,
            Filter::Mitchell => Self::Mitchell,
            Filter::CatmullRom => Self::CatmullRom,
            Filter::Bilinear => Self::Bilinear,
            Filter::Box => Self::Box,
            Filter::Nearest => Self::Nearest,
        }
    }
}

/// One key of [`Options`], as `options()` lists it: the shape of
/// [`crate::output::CodecOption`].
#[derive(Debug, Clone, Serialize, Tsify)]
pub struct OptionInfo {
    /// The key, as `optimize` and `encode` take it.
    pub key: &'static str,
    /// The value used when the key is not set, as the package spells it.
    /// `auto` where it depends on the image or the format. Left out for a
    /// key that has none: leaving `width` out resizes nothing, and
    /// `quality` left out means the `target` is searched for.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub default: Option<String>,
    /// What it does and which values it takes.
    pub help: &'static str,
}

/// A key of [`Options`] and what `options()` says about it.
struct Key {
    name: &'static str,
    help: &'static str,
}

/// The keys of [`Options`] as JavaScript spells them, with their help.
/// `serde-wasm-bindgen` reads the fields it knows and never sees the rest,
/// so a misspelt key is refused against these, and `options()` lists them.
/// Add a field's key here with the field.
const OPTIONS: &[Key] = &[
    Key {
        name: "format",
        help: "output format: jpeg, png, webp, avif or jxl. Left out, it is chosen per image: \
                avif for a photograph, lossless webp for a graphic",
    },
    Key {
        name: "target",
        help: "SSIMULACRA2 score to reach by searching encoder quality, up to 100. 100 is \
                identical, 50 shows artefacts on close inspection",
    },
    Key {
        name: "quality",
        help: "encode at this quality, 0 to 100, with no search. Excludes target and lossless",
    },
    Key {
        name: "lossless",
        help: "lossless output. Excludes target and quality",
    },
    Key {
        name: "preset",
        help: "named settings: web, thumbnail, archive or lossless. The other keys override it",
    },
    Key {
        name: "effort",
        help: "0 to 10. Higher is slower and smaller",
    },
    Key {
        name: "subsampling",
        help: "chroma subsampling, for codecs that have it: auto, 444, 422 or 420",
    },
    Key {
        name: "keepIcc",
        help: "keep the ICC profile instead of converting to sRGB",
    },
    Key {
        name: "keepMetadata",
        help: "keep EXIF and XMP instead of stripping them",
    },
    Key {
        name: "fast",
        help: "one encode at the calibrated seed quality for the target, no search",
    },
    Key {
        name: "width",
        help: "target width in pixels. With the default fit, scales down to fit inside width x \
                height; one side may be given alone",
    },
    Key {
        name: "height",
        help: "target height in pixels",
    },
    Key {
        name: "fit",
        help: "how the image meets the width x height box, as CSS object-fit: inside, cover, \
                contain, fill or outside",
    },
    Key {
        name: "position",
        help: "where cover crops and contain places the image: center, top, bottom, left, \
                right, top-left, top-right, bottom-left or bottom-right",
    },
    Key {
        name: "background",
        help: "padding of contain: #rrggbb, #rrggbbaa, white, black or transparent. auto is \
                transparent for a format with alpha, white for one without",
    },
    Key {
        name: "scale",
        help: "a factor instead of a box: 0.5 halves both sides. Above 1 needs enlarge",
    },
    Key {
        name: "enlarge",
        help: "allow scaling up. Nothing scales up without it",
    },
    Key {
        name: "filter",
        help: "resampling filter: lanczos3, mitchell, catmull-rom, bilinear, box or nearest",
    },
    Key {
        name: "maxPixels",
        help: "refuse an image above this many pixels, decoded or resized",
    },
    Key {
        name: "codecOpts",
        help: "backend options, { \"jpeg:progressive\": \"false\" }. codecs() lists every key \
                with its default",
    },
    Key {
        name: "onTrial",
        help: "called once per trial of the target search with { n, max, quality, score, size }",
    },
];
const DECODE_OPTIONS: &[&str] = &["maxPixels"];
const DECODE_ANY_OPTIONS: &[&str] = &["maxPixels", "width", "height"];

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidParams(message.into())
}

/// An options argument as its struct. A missing one is the defaults.
fn parse<T: DeserializeOwned + Default>(options: Option<JsValue>, keys: &[&str]) -> Result<T> {
    let Some(options) = options.filter(|o| !o.is_null() && !o.is_undefined()) else {
        return Ok(T::default());
    };
    if let Some(object) = options.dyn_ref::<Object>() {
        for key in Object::keys(object) {
            let key = key.as_string().unwrap_or_default();
            if !keys.contains(&key.as_str()) {
                return Err(invalid(format!(
                    "unknown option `{key}`; the options are {}",
                    keys.join(", ")
                )));
            }
        }
    }
    serde_wasm_bindgen::from_value(options).map_err(|e| invalid(format!("options: {e}")))
}

impl Options {
    /// The options a caller passed.
    ///
    /// # Errors
    /// [`Error::InvalidParams`] for a key this package does not have or a
    /// value of the wrong type.
    pub fn parse(options: Option<Ts<Self>>) -> Result<Self> {
        let keys: Vec<&str> = OPTIONS.iter().map(|k| k.name).collect();
        let options: Self = parse(options.map(Into::into), &keys)?;
        let on_trial = &options.on_trial;
        if !(on_trial.is_undefined() || on_trial.is_null() || on_trial.is_function()) {
            return Err(invalid("`onTrial` is not a function"));
        }
        Ok(options)
    }

    /// The builder these options stand for: the preset first, then the
    /// keys that override it, as the command line orders its flags.
    ///
    /// # Errors
    /// [`Error::InvalidParams`] for a value out of range, keys that
    /// exclude each other, or a codec option no encoder here takes.
    pub fn sqzer(&self) -> Result<Sqzer> {
        let mut sqzer = crate::sqzer(self.max_pixels);
        if let Some(p) = self.preset {
            sqzer = sqzer.preset(p.into());
        }
        let lossless = self.lossless.unwrap_or(false);
        let fast = self.fast.unwrap_or(false);
        let targets = [self.target.is_some(), self.quality.is_some(), lossless];
        if targets.iter().filter(|&&t| t).count() > 1 {
            return Err(invalid(
                "`target`, `quality` and `lossless` are alternatives: pass one",
            ));
        }
        if fast && (self.quality.is_some() || lossless) {
            return Err(invalid(
                "`fast` encodes at the seed quality of a `target`, so it cannot go with `quality` \
                 or `lossless`",
            ));
        }
        if let Some(t) = self.target {
            if !t.is_finite() || t > 100.0 {
                return Err(invalid(
                    "`target` is a SSIMULACRA2 score, a number up to 100",
                ));
            }
            sqzer = sqzer.target(Target::Ssimulacra2(t));
        }
        if let Some(q) = self.quality {
            if !(0.0..=100.0).contains(&q) {
                return Err(invalid("`quality` is 0 to 100"));
            }
            sqzer = sqzer.target(Target::Quality(q));
        }
        if lossless {
            sqzer = sqzer.target(Target::Lossless);
        }
        if let Some(e) = self.effort {
            if e > 10 {
                return Err(invalid("`effort` is 0 to 10"));
            }
            sqzer = sqzer.effort(e);
        }
        if let Some(s) = self.subsampling {
            sqzer = sqzer.subsampling(s.into());
        }
        if let Some(f) = self.format {
            sqzer = sqzer.format(f.into());
        }
        let resize = self.resize(sqzer.resize_bounds())?;
        self.codec_opts(
            sqzer
                .resize(resize)
                .keep_icc(self.keep_icc.unwrap_or(false))
                .keep_metadata(self.keep_metadata.unwrap_or(false))
                .fast(fast),
        )
    }

    /// The resize keys over the preset's resize. A size replaces the
    /// preset's box whole; the other keys modify whatever box there is, so
    /// `{ preset: "thumbnail", fit: "cover" }` crops to 512 x 512.
    fn resize(&self, preset: Resize) -> Result<Resize> {
        let mut r = preset;
        if let Some(factor) = self.scale {
            if self.width.is_some() || self.height.is_some() || self.fit.is_some() {
                return Err(invalid(
                    "`scale` sizes by a factor, so it cannot go with `width`, `height` or `fit`",
                ));
            }
            r = Resize {
                size: Size::Scale(factor),
                ..Resize::NONE
            };
        } else if self.width.is_some() || self.height.is_some() {
            r = Resize {
                size: Size::Box {
                    width: self.width,
                    height: self.height,
                },
                ..Resize::NONE
            };
        }
        if let Some(f) = self.fit {
            r.fit = f.into();
        }
        if let Some(p) = self.position {
            r.position = p.into();
        }
        if let Some(colour) = &self.background {
            r.background = Some(parse_colour(colour)?);
        }
        r.enlarge |= self.enlarge.unwrap_or(false);
        if let Some(f) = self.filter {
            r.filter = f.into();
        }
        if self.position.is_some() && !matches!(r.fit, resize::Fit::Cover | resize::Fit::Contain) {
            return Err(invalid(
                "`position` places a `cover` crop or a `contain` image, and `fit` is neither",
            ));
        }
        if self.background.is_some() && r.fit != resize::Fit::Contain {
            return Err(invalid(
                "`background` pads a `contain` image, and `fit` is not `contain`",
            ));
        }
        r.check()?;
        Ok(r)
    }

    /// `codecOpts` keys are checked against the backend that owns them.
    fn codec_opts(&self, mut sqzer: Sqzer) -> Result<Sqzer> {
        for (name, value) in &self.codec_opts {
            let (codec, key) = name
                .split_once(':')
                .filter(|(codec, key)| !codec.is_empty() && !key.is_empty())
                .ok_or_else(|| {
                    invalid(format!(
                        "`{name}`: a codec option is named `codec:key`, for example \
                         `jpeg:progressive`"
                    ))
                })?;
            let format = codec::Format::from_extension(codec)
                .filter(|f| !f.encoder_features().is_empty())
                .ok_or_else(|| invalid(format!("`{name}`: unknown codec `{codec}`")))?;
            let codec = format_name(format);
            let caps = sqzer
                .registry()
                .encoder(format)
                .map_err(|_| {
                    invalid(format!(
                        "`{name}`: no {format} encoder in this build to take it; `codecs()` \
                         shows what there is"
                    ))
                })?
                .caps();
            if !caps.options.iter().any(|o| o.key == key) {
                let known: Vec<String> = caps
                    .options
                    .iter()
                    .map(|o| format!("{codec}:{}", o.key))
                    .collect();
                let known = if known.is_empty() {
                    format!("{} takes no options", caps.name)
                } else {
                    format!("{} accepts {}", caps.name, known.join(", "))
                };
                return Err(invalid(format!(
                    "`{name}`: unknown {codec} option `{key}`; {known}"
                )));
            }
            sqzer = sqzer.codec_opt(codec, key, value.as_str());
        }
        Ok(sqzer)
    }

    /// The `onTrial` callback, if there is one.
    #[must_use]
    pub fn on_trial(&self) -> Option<&Function> {
        self.on_trial.dyn_ref()
    }

    /// Every key with its default and its help, for `options()`. The
    /// defaults are read from the builder the package starts from, so
    /// they cannot drift from what an encode without options does.
    #[must_use]
    pub fn listing() -> Vec<OptionInfo> {
        let sqzer = crate::sqzer(None);
        let params = sqzer.params();
        let resize = sqzer.resize_bounds();
        let default = |key: &str| -> Option<String> {
            Some(match key {
                // Decided per image or per format.
                "format" | "background" => "auto".to_string(),
                "target" => match params.target {
                    Target::Ssimulacra2(t) => crate::output::tidy(t).to_string(),
                    Target::Quality(_) | Target::Lossless => return None,
                },
                "lossless" | "fast" => false.to_string(),
                "effort" => params.effort.to_string(),
                "subsampling" => subsampling_name(params.subsampling).to_string(),
                "keepIcc" => params.keep_icc.to_string(),
                "keepMetadata" => params.keep_metadata.to_string(),
                "fit" => fit_name(resize.fit).to_string(),
                "position" => position_name(resize.position).to_string(),
                "enlarge" => resize.enlarge.to_string(),
                "filter" => filter_name(resize.filter).to_string(),
                "maxPixels" => MAX_PIXELS.to_string(),
                // Nothing happens when these are left out.
                _ => return None,
            })
        };
        OPTIONS
            .iter()
            .map(|k| OptionInfo {
                key: k.name,
                default: default(k.name),
                help: k.help,
            })
            .collect()
    }
}

/// The spelling of [`Subsampling`] for a library value.
fn subsampling_name(s: params::Subsampling) -> &'static str {
    match s {
        params::Subsampling::Auto => "auto",
        params::Subsampling::S444 => "444",
        params::Subsampling::S422 => "422",
        params::Subsampling::S420 => "420",
    }
}

/// The spelling of [`Fit`] for a library value.
fn fit_name(f: resize::Fit) -> &'static str {
    match f {
        resize::Fit::Inside => "inside",
        resize::Fit::Cover => "cover",
        resize::Fit::Contain => "contain",
        resize::Fit::Fill => "fill",
        resize::Fit::Outside => "outside",
    }
}

/// The spelling of [`Position`] for a library value.
fn position_name(p: resize::Position) -> &'static str {
    match p {
        resize::Position::Center => "center",
        resize::Position::Top => "top",
        resize::Position::Bottom => "bottom",
        resize::Position::Left => "left",
        resize::Position::Right => "right",
        resize::Position::TopLeft => "top-left",
        resize::Position::TopRight => "top-right",
        resize::Position::BottomLeft => "bottom-left",
        resize::Position::BottomRight => "bottom-right",
    }
}

/// The spelling of [`Filter`] for a library value.
fn filter_name(f: resize::Filter) -> &'static str {
    match f {
        resize::Filter::Lanczos3 => "lanczos3",
        resize::Filter::Mitchell => "mitchell",
        resize::Filter::CatmullRom => "catmull-rom",
        resize::Filter::Bilinear => "bilinear",
        resize::Filter::Box => "box",
        resize::Filter::Nearest => "nearest",
    }
}

impl DecodeOptions {
    /// The options a caller passed.
    ///
    /// # Errors
    /// See [`Options::parse`].
    pub fn parse(options: Option<Ts<Self>>) -> Result<Self> {
        parse(options.map(Into::into), DECODE_OPTIONS)
    }
}

impl DecodeAnyOptions {
    /// The options a caller passed.
    ///
    /// # Errors
    /// See [`Options::parse`].
    pub fn parse(options: Option<Ts<Self>>) -> Result<Self> {
        parse(options.map(Into::into), DECODE_ANY_OPTIONS)
    }
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa` or a name, to RGBA: the colours the
/// command line's `--background` takes.
fn parse_colour(s: &str) -> Result<[u8; 4]> {
    match s.to_ascii_lowercase().as_str() {
        "white" => return Ok([255; 4]),
        "black" => return Ok([0, 0, 0, 255]),
        "transparent" => return Ok([0; 4]),
        _ => {}
    }
    let bad = || {
        invalid(format!(
            "`{s}` is not a colour; try #rrggbb, #rrggbbaa, white, black or transparent"
        ))
    };
    let hex = s.strip_prefix('#').unwrap_or(s);
    if !hex.is_ascii() {
        return Err(bad());
    }
    let byte = |i: usize, len: usize| {
        u8::from_str_radix(&hex[i..i + len], 16)
            .map(|v| if len == 1 { v * 17 } else { v })
            .map_err(|_| bad())
    };
    match hex.len() {
        3 => Ok([byte(0, 1)?, byte(1, 1)?, byte(2, 1)?, 255]),
        6 => Ok([byte(0, 2)?, byte(2, 2)?, byte(4, 2)?, 255]),
        8 => Ok([byte(0, 2)?, byte(2, 2)?, byte(4, 2)?, byte(6, 2)?]),
        _ => Err(bad()),
    }
}
