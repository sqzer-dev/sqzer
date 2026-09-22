//! `--list-codecs`: what this build decodes and encodes, from which tier,
//! and with `-v` every backend's `--codec-opt` keys. Generated from the
//! registry, never maintained by hand.

use serde::Serialize;
use sqzer::core::Registry;
use sqzer::core::codec::{Format, Tier};

use crate::cli::format_name;

/// Human listing.
pub fn render(registry: &Registry, verbose: bool) -> String {
    let mut tiers: Vec<Tier> = registry
        .decoders()
        .map(|d| d.caps().tier)
        .chain(registry.encoders().map(|e| e.caps().tier))
        .collect();
    tiers.sort_by_key(ToString::to_string);
    tiers.dedup();
    let tiers: Vec<String> = tiers.iter().map(ToString::to_string).collect();
    let mut out = vec![format!(
        "sqzer {} - tiers in this build: {}",
        env!("CARGO_PKG_VERSION"),
        if tiers.is_empty() {
            "none".to_string()
        } else {
            tiers.join(", ")
        }
    )];
    out.push(String::new());
    for &format in Format::ALL {
        // A decoder that is compiled in but cannot run here is listed as
        // unavailable, with its reason on the next line.
        let mut unavailable = Vec::new();
        let decode = registry
            .decoders()
            .filter(|d| d.caps().format == format)
            .map(|d| match d.available() {
                Ok(()) => format!("{} ({})", d.caps().name, d.caps().tier),
                Err(reason) => {
                    unavailable.push(format!("{}: {reason}", d.caps().name));
                    format!("{} ({}, unavailable)", d.caps().name, d.caps().tier)
                }
            })
            .collect::<Vec<_>>();
        let decode = if decode.is_empty() {
            none_because(format.decoder_features())
        } else {
            decode.join(", ")
        };
        let encode = if let Ok(e) = registry.encoder(format) {
            {
                let c = e.caps();
                let mode = match (c.lossy, c.lossless) {
                    (true, true) => "lossy and lossless".to_string(),
                    (true, false) => "lossy".to_string(),
                    (false, true) => {
                        let native: Vec<&str> = format
                            .encoder_features()
                            .iter()
                            .copied()
                            .filter(|f| f.starts_with("native-"))
                            .collect();
                        if native.is_empty() {
                            "lossless".to_string()
                        } else {
                            format!("lossless only; lossy needs `{}`", native.join("` or `"))
                        }
                    }
                    (false, false) => "no mode".to_string(),
                };
                let metadata = match (c.exif, c.xmp) {
                    (true, true) => "EXIF and XMP",
                    (true, false) => "EXIF only",
                    (false, true) => "XMP only",
                    (false, false) => "none",
                };
                format!("{} ({}), {mode}, metadata: {metadata}", c.name, c.tier)
            }
        } else {
            none_because(format.encoder_features())
        };
        out.push(format!(
            "{:<8} decode  {:<28} encode  {encode}",
            format.to_string(),
            decode
        ));
        for reason in unavailable {
            out.push(format!("{:<16} {reason}", ""));
        }
        if verbose && let Ok(e) = registry.encoder(format) {
            let codec = format_name(format);
            for opt in e.caps().options {
                out.push(format!(
                    "{:<8} option  {codec}:{}={}",
                    "", opt.key, opt.default
                ));
                out.push(format!("{:<16} {}", "", opt.help));
            }
        }
    }
    out.join("\n")
}

/// One `--json` line per format.
#[derive(Serialize)]
struct Line {
    format: &'static str,
    extension: &'static str,
    mime: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    decoder: Option<DecoderLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoder: Option<EncoderLine>,
    decoder_features: &'static [&'static str],
    encoder_features: &'static [&'static str],
}

/// The decoder that would read the format: the first usable one, or, when
/// none can run here, the first compiled in, with `available: false` and
/// the reason.
#[derive(Serialize)]
struct DecoderLine {
    backend: &'static str,
    tier: String,
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct EncoderLine {
    backend: &'static str,
    tier: String,
    lossy: bool,
    lossless: bool,
    alpha: bool,
    animation: bool,
    /// Can embed EXIF under `--keep-metadata`.
    exif: bool,
    /// Can embed XMP under `--keep-metadata`.
    xmp: bool,
    bit_depth: &'static [u8],
    options: Vec<OptionLine>,
}

#[derive(Serialize)]
struct OptionLine {
    key: String,
    default: &'static str,
    help: &'static str,
}

/// The `none` cell: which feature would add the backend, or, in a native
/// build whose target leaves it out, why and which archive has it.
fn none_because(features: &[&str]) -> String {
    if features.is_empty() {
        return "none".to_string();
    }
    let left_out: Vec<&str> = features
        .iter()
        .filter_map(|f| crate::native_set::left_out(f))
        .collect();
    if left_out.is_empty() {
        format!("none; needs `{}`", features.join("` or `"))
    } else {
        format!("none; {}", left_out.join("; "))
    }
}

/// JSON Lines listing.
pub fn render_json(registry: &Registry) -> String {
    let mut out = Vec::new();
    for &format in Format::ALL {
        let candidates: Vec<_> = registry
            .decoders()
            .filter(|d| d.caps().format == format)
            .map(|d| (d, d.available()))
            .collect();
        let decoder = candidates
            .iter()
            .find(|(_, available)| available.is_ok())
            .or_else(|| candidates.first())
            .map(|(d, available)| DecoderLine {
                backend: d.caps().name,
                tier: d.caps().tier.to_string(),
                available: available.is_ok(),
                reason: available.clone().err(),
            });
        let encoder = registry.encoder(format).ok().map(|e| {
            let c = e.caps();
            EncoderLine {
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
                    .map(|o| OptionLine {
                        key: format!("{}:{}", format_name(format), o.key),
                        default: o.default,
                        help: o.help,
                    })
                    .collect(),
            }
        });
        let line = Line {
            format: format_name(format),
            extension: format.extension(),
            mime: format.mime(),
            decoder,
            encoder,
            decoder_features: format.decoder_features(),
            encoder_features: format.encoder_features(),
        };
        if let Ok(s) = serde_json::to_string(&line) {
            out.push(s);
        }
    }
    out.join("\n")
}

#[cfg(all(test, feature = "portable"))]
mod tests {
    use super::*;

    #[test]
    fn listing_is_generated_from_the_registry() {
        let reg = sqzer::codecs::registry();
        let text = render(&reg, true);
        assert!(
            text.contains("JPEG     decode  zune-jpeg (portable)"),
            "{text}"
        );
        assert!(
            text.contains("JPEG XL  decode  jxl-oxide (portable)"),
            "{text}"
        );
        assert!(text.contains("png:interlace=false"), "{text}");
        // What a `native` build carries depends on the target, see
        // `native_set`.
        if crate::native_set::JPEGLI {
            assert!(text.contains("jpegli (native), lossy"), "{text}");
        } else {
            assert!(text.contains("mozjpeg-rs (portable), lossy"), "{text}");
            assert!(text.contains("jpeg:progressive=true"), "{text}");
        }
        if crate::native_set::JXL {
            assert!(
                text.contains("gamut-jxl (native), lossy and lossless"),
                "{text}"
            );
            assert!(text.contains("jxl:container=false"), "{text}");
        } else if crate::native_set::NATIVE {
            // musl: the reason, not the feature the user cannot enable.
            assert!(text.contains("none; the static musl build"), "{text}");
            assert!(!text.contains("needs `native-jxl`"), "{text}");
        } else {
            assert!(text.contains("none; needs `native-jxl`"), "{text}");
        }
        // Which HEIC decoder a build has depends on the target; the OS
        // decoder is listed first where there is one.
        let heic = if !crate::native_set::HEIC && crate::native_set::NATIVE {
            "HEIC     decode  none; a static musl binary"
        } else if !crate::native_set::HEIC {
            "HEIC     decode  none; needs `native-heif`"
        } else if cfg!(target_os = "macos") {
            "HEIC     decode  imageio (native"
        } else if cfg!(windows) {
            "HEIC     decode  wic (native"
        } else {
            "HEIC     decode  libheif (native"
        };
        assert!(text.contains(heic), "{text}");
        assert!(!render(&reg, false).contains("option"));
        for line in render_json(&reg).lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v["format"].is_string());
            if v["format"] == "heic" {
                assert_eq!(v["decoder_features"], serde_json::json!(["native-heif"]));
                if !crate::native_set::HEIC {
                    assert!(v["decoder"].is_null(), "{v}");
                }
            }
        }
    }

    #[test]
    fn unavailable_decoders_are_listed_with_their_reason() {
        use sqzer::core::codec::{Decoder, DecoderCaps, FormatInfo};
        use sqzer::core::image::Image;
        use sqzer::core::params::DecodeOpts;

        struct Missing;
        impl Decoder for Missing {
            fn caps(&self) -> &DecoderCaps {
                static CAPS: DecoderCaps = DecoderCaps {
                    format: Format::Heic,
                    name: "libheif",
                    animation: false,
                    tier: Tier::Native,
                };
                &CAPS
            }
            fn available(&self) -> Result<(), String> {
                Err("libheif.so.1 not found".into())
            }
            fn probe(&self, _: &[u8]) -> Option<FormatInfo> {
                None
            }
            fn dimensions(&self, _: &[u8]) -> Option<(u32, u32)> {
                None
            }
            fn decode(&self, _: &[u8], _: &DecodeOpts) -> sqzer::core::Result<Image> {
                unreachable!()
            }
        }

        let mut reg = Registry::new();
        reg.register_decoder(Missing);
        let text = render(&reg, false);
        assert!(
            text.contains("HEIC     decode  libheif (native, unavailable)"),
            "{text}"
        );
        assert!(
            text.contains("\n                 libheif: libheif.so.1 not found"),
            "{text}"
        );
        let heic = render_json(&reg)
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v["format"] == "heic")
            .unwrap();
        assert_eq!(heic["decoder"]["backend"], "libheif");
        assert_eq!(heic["decoder"]["available"], false);
        assert_eq!(heic["decoder"]["reason"], "libheif.so.1 not found");
    }
}
