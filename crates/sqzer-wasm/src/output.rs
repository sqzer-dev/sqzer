//! What the package hands back: the `--json` record of ADR-0003 in camel
//! case with the encoded bytes added, and the `--list-codecs --json`
//! listing, so the same words mean the same things in both.

use serde::{Serialize, Serializer};
use sqzer::core::codec::Format;
use sqzer::core::content::Content;
use sqzer::core::params::{Resolved, Target};
use sqzer::core::{Decoded, Registry};
use tsify::Tsify;

use crate::format_name;

/// A decoded image as the record describes it.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    /// What the input was detected as. `None` for pixels handed in.
    pub format: Option<Format>,
    /// The input has more than one frame.
    pub animated: bool,
    /// Width after orientation.
    pub width: u32,
    /// Height after orientation.
    pub height: u32,
    /// The input has an alpha channel.
    pub alpha: bool,
}

impl Source {
    /// The facts of `decoded`.
    #[must_use]
    pub fn of(decoded: &Decoded) -> Self {
        Self {
            format: Some(decoded.info.format),
            animated: decoded.info.animated,
            width: decoded.image.width(),
            height: decoded.image.height(),
            alpha: decoded.image.has_alpha(),
        }
    }
}

/// What `optimize` and `SqzerImage.encode` return: the encoded file and
/// how it was made. Keys that do not apply are left out, never null.
#[derive(Debug, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    /// The encoded file.
    #[serde(serialize_with = "as_bytes")]
    #[tsify(type = "Uint8Array")]
    pub bytes: Vec<u8>,
    /// Detected input format, as `format` spells it. Left out for an image
    /// made by `fromPixels`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub input_format: Option<&'static str>,
    /// The input has more than one frame. The first is what was encoded.
    pub animated: bool,
    /// Input width after orientation.
    pub width: u32,
    /// Input height after orientation.
    pub height: u32,
    /// The input has an alpha channel.
    pub alpha: bool,
    /// `photo` or `graphic`, the content class that picks the default
    /// format.
    pub content: &'static str,
    /// Output format.
    pub format: &'static str,
    /// Width of the output: `width` after the resize, a `contain` fit's
    /// padding included.
    pub output_width: u32,
    /// Height of the output.
    pub output_height: u32,
    /// Backend crate that wrote the output.
    pub backend: &'static str,
    /// Its tier.
    pub tier: String,
    /// Abstract quality the encoder ran with. Left out when the encoder
    /// ran in its lossless mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub quality: Option<f64>,
    /// The output holds exactly the samples the encoder was given. `false`
    /// at a quality, and under an option that gives samples up, such as
    /// `png:colors`.
    pub lossless: bool,
    /// SSIMULACRA2 target that was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub target: Option<f64>,
    /// SSIMULACRA2 score of the output, when the target was searched.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub score: Option<f64>,
    /// The score is at or above the target.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub reached: Option<bool>,
    /// The search hit the quality ceiling and still fell short.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub capped: Option<bool>,
    /// Encodes the search performed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub iterations: Option<u8>,
    /// Every trial in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub trials: Option<Vec<Trial>>,
}

/// One quality the search tried.
#[derive(Debug, Clone, Copy, Serialize, Tsify)]
pub struct Trial {
    /// Abstract quality.
    pub quality: f64,
    /// SSIMULACRA2 score.
    pub score: f64,
}

/// What `onTrial` receives: a trial as it is scored.
#[derive(Debug, Clone, Copy, Serialize, Tsify)]
pub struct TrialProgress {
    /// Position in the budget, from one.
    pub n: u8,
    /// The budget: the most encodes the search makes.
    pub max: u8,
    /// Abstract quality tried.
    pub quality: f64,
    /// SSIMULACRA2 score it reached.
    pub score: f64,
}

impl Output {
    /// The record of `out`, encoded from `source` under `target`.
    #[must_use]
    pub fn new(source: &Source, out: sqzer::Output, target: &Target) -> Self {
        let quality = match out.target {
            Resolved::Quality(q) => Some(tidy(q)),
            Resolved::Lossless => None,
        };
        let lossless = out.lossless;
        let report = out.report.as_ref();
        Self {
            input_format: source.format.map(format_name),
            animated: source.animated,
            width: source.width,
            height: source.height,
            alpha: source.alpha,
            content: match out.content {
                Content::Photo => "photo",
                Content::Graphic => "graphic",
            },
            format: format_name(out.format),
            output_width: out.width,
            output_height: out.height,
            backend: out.backend,
            tier: out.tier.to_string(),
            quality,
            lossless,
            target: match target {
                Target::Ssimulacra2(t) => Some(tidy(*t)),
                Target::Quality(_) | Target::Lossless => None,
            },
            score: report.map(|r| tidy(r.score)),
            reached: report.map(|r| r.reached),
            capped: report.map(|r| r.capped),
            iterations: report.map(|r| r.iterations),
            trials: report.map(|r| {
                r.trials
                    .iter()
                    .map(|t| Trial {
                        quality: tidy(t.quality),
                        score: tidy(t.score),
                    })
                    .collect()
            }),
            bytes: out.bytes,
        }
    }
}

/// An `f32` as the JavaScript number that prints the same digits: 51.8,
/// where a plain widening reads 51.79999923706055. `--json` prints the
/// short form too.
#[must_use]
pub fn tidy(v: f32) -> f64 {
    v.to_string().parse().unwrap_or_else(|_| f64::from(v))
}

/// A `Uint8Array`, where a `Vec<u8>` would cross as an array of numbers.
fn as_bytes<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bytes(bytes)
}

/// One format of the `codecs()` listing.
#[derive(Debug, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct Codec {
    /// The format, as `format` spells it.
    pub format: &'static str,
    /// Canonical file extension, without the dot.
    pub extension: &'static str,
    /// MIME type.
    pub mime: &'static str,
    /// The decoder that reads the format. Left out when this build has
    /// none; `decodeAny` then asks the browser.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub decoder: Option<DecoderInfo>,
    /// The encoder that writes the format. Left out when this build has
    /// none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[tsify(optional)]
    pub encoder: Option<EncoderInfo>,
    /// Cargo features of a `sqzer` build that has a decoder.
    pub decoder_features: &'static [&'static str],
    /// Cargo features of a `sqzer` build that has an encoder. Empty for an
    /// input-only format.
    pub encoder_features: &'static [&'static str],
}

/// A decoder of the `codecs()` listing.
#[derive(Debug, Serialize, Tsify)]
pub struct DecoderInfo {
    /// Backend crate.
    pub backend: &'static str,
    /// Its tier.
    pub tier: String,
}

/// An encoder of the `codecs()` listing.
#[derive(Debug, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
pub struct EncoderInfo {
    /// Backend crate.
    pub backend: &'static str,
    /// Its tier.
    pub tier: String,
    /// Writes lossy output.
    pub lossy: bool,
    /// Writes lossless output.
    pub lossless: bool,
    /// Writes an alpha channel.
    pub alpha: bool,
    /// Writes animation.
    pub animation: bool,
    /// Can embed EXIF under `keepMetadata`.
    pub exif: bool,
    /// Can embed XMP under `keepMetadata`.
    pub xmp: bool,
    /// Bit depths it accepts.
    pub bit_depth: &'static [u8],
    /// Its `codecOpts` keys.
    pub options: Vec<CodecOption>,
}

/// One `codecOpts` key.
#[derive(Debug, Serialize, Tsify)]
pub struct CodecOption {
    /// The key, `codec:name`.
    pub key: String,
    /// The value used when the key is not set.
    pub default: &'static str,
    /// What it does and which values it takes.
    pub help: &'static str,
}

/// The listing of `registry`, one entry per format `sqzer` knows.
#[must_use]
pub fn codecs(registry: &Registry) -> Vec<Codec> {
    Format::ALL
        .iter()
        .map(|&format| {
            let decoder = registry
                .decoders()
                .find(|d| d.caps().format == format && d.available().is_ok())
                .map(|d| DecoderInfo {
                    backend: d.caps().name,
                    tier: d.caps().tier.to_string(),
                });
            let encoder = registry.encoder(format).ok().map(|e| {
                let c = e.caps();
                EncoderInfo {
                    backend: c.name,
                    tier: c.tier.to_string(),
                    lossy: c.lossy,
                    lossless: c.lossless,
                    alpha: c.alpha,
                    animation: c.animation,
                    exif: c.exif,
                    xmp: c.xmp,
                    bit_depth: c.bit_depth,
                    options: c
                        .options
                        .iter()
                        .map(|o| CodecOption {
                            key: format!("{}:{}", format_name(format), o.key),
                            default: o.default,
                            help: o.help,
                        })
                        .collect(),
                }
            });
            Codec {
                format: format_name(format),
                extension: format.extension(),
                mime: format.mime(),
                decoder,
                encoder,
                decoder_features: format.decoder_features(),
                encoder_features: format.encoder_features(),
            }
        })
        .collect()
}
