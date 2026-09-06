//! Codec backends. Each module is gated by a Cargo feature; see `Cargo.toml`
//! for the tier layout.
//!
//! Rule: nothing in this crate may pull in an AGPL dependency. `cargo deny`
//! enforces it. AGPL backends belong in a separate `sqzer-codecs-agpl` crate.
//!
//! Registration order matters: [`register_portable`] runs before
//! [`register_native`], so a native backend takes over a format when both
//! are compiled in. See [`Registry`] for the precedence rule.

pub use sqzer_core::Registry;

#[cfg(feature = "avif")]
pub mod avif;
#[cfg(any(feature = "jpeg", feature = "webp-lossless"))]
mod exif;
#[cfg(feature = "jpeg")]
pub mod jpeg;
#[cfg(feature = "jxl-decode")]
pub mod jxl;
#[cfg(any(
    feature = "jpeg",
    feature = "webp-lossless",
    feature = "avif",
    all(feature = "png", not(target_arch = "wasm32"))
))]
mod opts;
#[cfg(all(feature = "png", not(target_arch = "wasm32")))]
pub mod oxipng;
#[cfg(feature = "png")]
pub mod png;
#[cfg(feature = "webp-lossless")]
pub mod webp;

/// Every backend enabled by the active feature set, portable tier first.
#[must_use]
pub fn registry() -> Registry {
    let mut reg = Registry::new();
    register_portable(&mut reg);
    register_native(&mut reg);
    reg
}

/// Add the compiled-in portable (pure Rust, permissive) backends.
pub fn register_portable(reg: &mut Registry) {
    #[cfg(feature = "jpeg")]
    {
        reg.register_decoder(jpeg::JpegDecoder);
        reg.register_encoder(jpeg::MozjpegEncoder);
    }
    #[cfg(feature = "png")]
    {
        reg.register_decoder(png::PngDecoder);
        // `oxipng` carries C (`libdeflate`), so wasm32 gets the plain
        // `png` writer instead. ADR-0002.
        #[cfg(not(target_arch = "wasm32"))]
        reg.register_encoder(oxipng::OxipngEncoder);
        #[cfg(target_arch = "wasm32")]
        reg.register_encoder(png::PngEncoder);
    }
    #[cfg(feature = "webp-lossless")]
    {
        reg.register_decoder(webp::WebPDecoder);
        reg.register_encoder(webp::WebPLosslessEncoder);
    }
    #[cfg(feature = "avif")]
    {
        #[cfg(not(target_arch = "wasm32"))]
        reg.register_decoder(avif::AvifDecoder);
        reg.register_encoder(avif::RavifEncoder);
    }
    #[cfg(feature = "jxl-decode")]
    {
        reg.register_decoder(jxl::JxlDecoder);
    }
    // Silence the unused-variable lint when no portable feature is on.
    let _ = reg;
}

/// Add the compiled-in native (C binding) backends. Empty until ADR-0001
/// item 8.
pub fn register_native(reg: &mut Registry) {
    let _ = reg;
}
