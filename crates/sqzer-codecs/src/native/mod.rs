//! Native tier: C codecs behind opt-in features, ADR-0004, and the HEIC
//! decoders of ADR-0005. Each module adapts one library to the core
//! traits; nothing here is compiled unless its feature is on, and none of
//! it builds for wasm32.
//!
//! Registration order in [`crate::register_native`] is WebP, JPEG XL,
//! AVIF, HEIC (OS decoder first, then `libheif`), JPEG. A native encoder
//! registers after the portable one for its format and takes it over, per
//! the [`crate::Registry`] precedence rule.

#[cfg(feature = "native-avif")]
pub mod avif;
// A static musl binary cannot load `libheif` at run time and has no OS
// decoder, so the feature registers nothing there (ADR-0005 D8).
#[cfg(all(feature = "native-heif", not(target_env = "musl")))]
pub mod heif;
#[cfg(feature = "native-jpegli")]
pub mod jpegli;
#[cfg(feature = "native-jxl")]
pub mod jxl;
#[cfg(feature = "native-webp")]
pub mod webp;
