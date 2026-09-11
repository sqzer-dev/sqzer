//! Parsers for `codec_specific` values, shared by the backends. Error
//! messages name the option as the user wrote it, `codec:key`.

// Each backend uses a different subset, so any single-feature build leaves
// some of these unused.
#![allow(dead_code)]

use sqzer_core::{Error, Result};

/// `true` / `false`, `1` / `0`, `yes` / `no`, `on` / `off`.
pub(crate) fn parse_bool(codec: &str, key: &str, value: &str) -> Result<bool> {
    match value {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(Error::InvalidParams(format!(
            "{codec}:{key} expects a boolean, got `{value}`"
        ))),
    }
}

/// An integer `0..=255`.
pub(crate) fn parse_u8(codec: &str, key: &str, value: &str) -> Result<u8> {
    value.parse().map_err(|_| {
        Error::InvalidParams(format!(
            "{codec}:{key} expects an integer 0..=255, got `{value}`"
        ))
    })
}

/// An integer `0..=100`.
pub(crate) fn parse_percent(codec: &str, key: &str, value: &str) -> Result<u8> {
    value
        .parse::<u8>()
        .ok()
        .filter(|v| *v <= 100)
        .ok_or_else(|| {
            Error::InvalidParams(format!(
                "{codec}:{key} expects an integer 0..=100, got `{value}`"
            ))
        })
}

/// The option is owned by `codec` but not known to it.
pub(crate) fn unknown(codec: &str, key: &str) -> Error {
    Error::InvalidParams(format!("unknown {codec} option `{key}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn booleans_accept_the_usual_spellings() {
        assert!(parse_bool("jpeg", "progressive", "on").unwrap());
        assert!(!parse_bool("jpeg", "progressive", "off").unwrap());
        let err = parse_bool("jpeg", "progressive", "maybe").unwrap_err();
        assert!(err.to_string().contains("jpeg:progressive"), "{err}");
    }

    #[test]
    fn integers_are_bounded() {
        assert_eq!(parse_u8("jpeg", "smoothing", "7").unwrap(), 7);
        assert!(parse_u8("jpeg", "smoothing", "256").is_err());
        assert!(parse_u8("jpeg", "smoothing", "x").is_err());
    }

    #[test]
    fn percentages_stop_at_100() {
        assert_eq!(parse_percent("webp", "alpha_quality", "100").unwrap(), 100);
        assert_eq!(parse_percent("webp", "alpha_quality", "0").unwrap(), 0);
        assert!(parse_percent("webp", "alpha_quality", "101").is_err());
        assert!(parse_percent("webp", "alpha_quality", "-1").is_err());
    }
}
