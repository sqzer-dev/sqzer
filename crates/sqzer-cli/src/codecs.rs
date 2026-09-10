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
        let decode = registry
            .decoders()
            .filter(|d| d.caps().format == format)
            .map(|d| format!("{} ({})", d.caps().name, d.caps().tier))
            .collect::<Vec<_>>();
        let decode = if decode.is_empty() {
            "none".to_string()
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
                format!("{} ({}), {mode}", c.name, c.tier)
            }
        } else {
            let features = format.encoder_features();
            if features.is_empty() {
                "none".to_string()
            } else {
                format!("none; needs `{}`", features.join("` or `"))
            }
        };
        out.push(format!(
            "{:<8} decode  {:<28} encode  {encode}",
            format.to_string(),
            decode
        ));
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
    decoder: Option<Backend>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoder: Option<EncoderLine>,
    encoder_features: &'static [&'static str],
}

#[derive(Serialize)]
struct Backend {
    backend: &'static str,
    tier: String,
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
    bit_depth: &'static [u8],
    options: Vec<OptionLine>,
}

#[derive(Serialize)]
struct OptionLine {
    key: String,
    default: &'static str,
    help: &'static str,
}

/// JSON Lines listing.
pub fn render_json(registry: &Registry) -> String {
    let mut out = Vec::new();
    for &format in Format::ALL {
        let decoder = registry
            .decoders()
            .rev()
            .find(|d| d.caps().format == format)
            .map(|d| Backend {
                backend: d.caps().name,
                tier: d.caps().tier.to_string(),
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
        assert!(text.contains("mozjpeg-rs (portable), lossy"), "{text}");
        assert!(text.contains("jpeg:progressive=true"), "{text}");
        assert!(
            text.contains("JPEG XL  decode  jxl-oxide (portable)"),
            "{text}"
        );
        assert!(text.contains("none; needs `native-jxl`"), "{text}");
        assert!(!render(&reg, false).contains("option"));
        for line in render_json(&reg).lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v["format"].is_string());
        }
    }
}
