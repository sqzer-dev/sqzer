//! Native tier: C codecs behind opt-in features, ADR-0004. Each module
//! adapts one library to the core traits; nothing here is compiled unless
//! its feature is on, and none of it builds for wasm32.
//!
//! Registration order in [`crate::register_native`] is WebP, JPEG XL,
//! AVIF, HEIC, JPEG. A native encoder registers after the portable one
//! for its format and takes it over, per the [`crate::Registry`]
//! precedence rule.

#[cfg(feature = "native-avif")]
pub mod avif;
#[cfg(feature = "native-heif")]
pub mod heif;
#[cfg(feature = "native-jpegli")]
pub mod jpegli;
#[cfg(feature = "native-jxl")]
pub mod jxl;
#[cfg(feature = "native-webp")]
pub mod webp;
