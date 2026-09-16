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

#[cfg(all(
    target_arch = "wasm32",
    any(
        feature = "native-webp",
        feature = "native-jxl",
        feature = "native-avif",
        feature = "native-heif",
        feature = "native-jpegli",
    )
))]
compile_error!("the native tier is C code and does not build for wasm32; use the portable tier");

#[cfg(feature = "avif")]
pub mod avif;
#[cfg(any(feature = "jpeg", feature = "webp-lossless"))]
mod exif;
#[cfg(feature = "gif")]
pub mod gif;
#[cfg(feature = "heif")]
pub mod heif;
#[cfg(feature = "jpeg")]
pub mod jpeg;
#[cfg(feature = "jxl-decode")]
pub mod jxl;
#[cfg(any(feature = "avif", feature = "native-webp", feature = "native-avif"))]
mod layout;
#[cfg(any(
    feature = "native-webp",
    feature = "native-jxl",
    feature = "native-avif",
    feature = "native-heif",
    feature = "native-jpegli",
))]
pub mod native;
#[cfg(any(
    feature = "jpeg",
    feature = "webp-lossless",
    feature = "avif",
    all(feature = "png", not(target_arch = "wasm32")),
    feature = "native-webp",
    feature = "native-jxl",
    feature = "native-avif",
    feature = "native-jpegli",
))]
mod opts;
#[cfg(all(feature = "png", not(target_arch = "wasm32")))]
pub mod oxipng;
#[cfg(feature = "png")]
pub mod png;
#[cfg(any(
    feature = "bmp",
    feature = "tga",
    feature = "ico",
    feature = "qoi",
    feature = "pnm"
))]
pub mod raster;
#[cfg(feature = "svg")]
pub mod svg;
#[cfg(feature = "tiff")]
pub mod tiff;
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
    // Input-only formats. TGA registers last of all: it has no magic
    // number and its probe is a plausibility check, so every format with
    // one gets asked first.
    #[cfg(feature = "gif")]
    reg.register_decoder(gif::GifDecoder);
    #[cfg(feature = "tiff")]
    reg.register_decoder(tiff::TiffDecoder);
    #[cfg(feature = "bmp")]
    reg.register_decoder(raster::BmpDecoder);
    #[cfg(feature = "ico")]
    reg.register_decoder(raster::IcoDecoder);
    #[cfg(feature = "qoi")]
    reg.register_decoder(raster::QoiDecoder);
    #[cfg(feature = "pnm")]
    reg.register_decoder(raster::PnmDecoder);
    #[cfg(feature = "svg")]
    reg.register_decoder(svg::SvgDecoder);
    #[cfg(feature = "tga")]
    reg.register_decoder(raster::TgaDecoder);
    // HEIC is recognised in every build so the error for one names the
    // feature that reads it. The sniffer is only consulted when no
    // decoder claims the bytes, so a `native-heif` build is unaffected.
    #[cfg(feature = "heif")]
    reg.register_sniffer(heif::probe);
    // Silence the unused-variable lint when no portable feature is on.
    let _ = reg;
}

/// Add the compiled-in native (C binding) backends. Each encoder takes
/// over its format from the portable tier; the HEIC decoders add a format
/// the portable tier does not read. On macOS and Windows the OS decoder
/// registers first and the runtime-loaded `libheif` second, so the
/// zero-install path is tried first and `libheif` covers a machine whose
/// OS decoder is missing (ADR-0005 D1). Linux has `libheif` only; musl,
/// which cannot load a library at run time, has nothing.
pub fn register_native(reg: &mut Registry) {
    #[cfg(feature = "native-webp")]
    reg.register_encoder(native::webp::LibwebpEncoder);
    #[cfg(feature = "native-jxl")]
    reg.register_encoder(native::jxl::LibjxlEncoder);
    #[cfg(feature = "native-avif")]
    reg.register_encoder(native::avif::LibavifEncoder);
    #[cfg(all(feature = "native-heif", target_os = "macos"))]
    reg.register_decoder(native::heif::ImageIoDecoder);
    #[cfg(all(feature = "native-heif", windows))]
    reg.register_decoder(native::heif::WicDecoder);
    #[cfg(all(feature = "native-heif", not(target_env = "musl")))]
    reg.register_decoder(native::heif::LibheifDecoder);
    #[cfg(feature = "native-jpegli")]
    reg.register_encoder(native::jpegli::JpegliEncoder);
    // Silence the unused-variable lint when no native feature is on.
    let _ = reg;
}
