//! The browser build of `sqzer`: the `sqzer` package on npm, ADR-0011 D3.
//! The API is the library's builder spelled the way JavaScript spells it.
//!
//! ```js
//! import init, { optimize, decode, decodeAny, fromPixels, codecs } from "sqzer";
//! await init();
//!
//! // everything in one call: probe, decode, prepare, resize, encode
//! const out = optimize(bytes, { format: "avif", target: 70, width: 1600 });
//!
//! // decode once, encode as often as a slider moves
//! const image = decode(bytes);
//! const a = image.encode({ format: "webp", lossless: true });
//! const b = image.encode({ format: "jpeg", quality: 80, onTrial: (t) => {} });
//! image.free();
//! ```
//!
//! The package carries the portable tier minus `svg`, is synchronous and
//! spawns no thread; a page runs it in a worker. Options are plain objects
//! whose keys are the command line's flags in camel case, see
//! [`options::Options`], and a result is the `--json` record with `bytes`
//! added, see [`output::Output`]. Their TypeScript types are generated
//! from those structs.

mod error;
pub mod options;
pub mod output;
#[cfg(all(test, target_arch = "wasm32"))]
// JavaScript numbers holding pixel counts and decimals that cross exactly.
#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
mod tests;

use std::cell::Cell;

use sqzer::core::codec::{Format, FormatInfo};
use sqzer::core::image::{ColorType, Image};
use sqzer::core::params::Resize;
use sqzer::core::resize::Size;
use sqzer::core::{Decoded, Error, Registry};
use sqzer::{Progress, Ready, Sqzer};
use wasm_bindgen::prelude::*;

pub use error::SqzerError;
use options::{DecodeAnyOptions, DecodeOptions, MAX_PIXELS, Options};
use output::{Codec, Output, Source, TrialProgress, tidy};
use tsify::{Ts, Tsify};

#[wasm_bindgen(module = "/js/canvas.js")]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn open(bytes: &[u8], mime: &str) -> Result<JsValue, JsValue>;
    fn size(source: &JsValue) -> Vec<u32>;
    #[wasm_bindgen(catch)]
    fn rasterise(source: &JsValue, width: u32, height: u32) -> Result<Vec<u8>, JsValue>;
    fn close(source: &JsValue);
}

// Runs when the module is instantiated. Not part of the API, so not in
// the type declarations.
#[wasm_bindgen(start, skip_typescript)]
fn start() {
    console_error_panic_hook::set_once();
}

/// The name `format` and `codecOpts` use for a format, which is the
/// command line's.
fn format_name(format: Format) -> &'static str {
    match format {
        Format::Jpeg => "jpeg",
        other => other.extension(),
    }
}

/// Every backend of the build, and SVG recognised without a decoder, the
/// way a portable build recognises HEIC: `decode` then names the format,
/// and `decodeAny` knows to ask the browser.
fn registry() -> Registry {
    let mut registry = sqzer::codecs::registry();
    registry.register_sniffer(sniff_svg);
    registry
}

/// The builder every call starts from.
fn sqzer(max_pixels: Option<u32>) -> Sqzer {
    Sqzer::with_registry(registry()).max_pixels(u64::from(max_pixels.unwrap_or(MAX_PIXELS)))
}

/// Whether the first element of the document is `<svg`, after an optional
/// byte-order mark, XML declaration, comments and a doctype. The sniff of
/// the SVG decoder in `sqzer-codecs`, which this build leaves out.
fn sniff_svg(bytes: &[u8]) -> Option<FormatInfo> {
    let mut s = &bytes[..bytes.len().min(4096)];
    s = s.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(s);
    loop {
        s = s.trim_ascii_start();
        let end: &[u8] = if s.starts_with(b"<?") {
            b"?>"
        } else if s.starts_with(b"<!--") {
            b"-->"
        } else if s.starts_with(b"<!") {
            b">"
        } else {
            break;
        };
        let at = s.windows(end.len()).position(|w| w == end)?;
        s = &s[at + end.len()..];
    }
    s.strip_prefix(b"<svg")?
        .first()
        .is_some_and(|c| c.is_ascii_whitespace() || matches!(c, b'>' | b'/'))
        .then_some(FormatInfo {
            format: Format::Svg,
            animated: false,
        })
}

/// Probe, decode, prepare, resize and encode `bytes` in one call.
///
/// Without options the output is the default of the `sqzer` command line:
/// AVIF for a photograph, lossless WebP for a graphic, encoder quality
/// searched until the output scores 70 on SSIMULACRA2, metadata stripped,
/// colours converted to sRGB.
///
/// # Errors
/// Throws a `SqzerError`, whose `kind` says which failure it is.
#[wasm_bindgen]
pub fn optimize(bytes: &[u8], options: Option<Ts<Options>>) -> Result<Ts<Output>, SqzerError> {
    let options = Options::parse(options)?;
    let sqzer = options.sqzer()?;
    let decoded = sqzer.decode(bytes)?;
    let source = Source::of(&decoded);
    let ready = sqzer.transform(decoded)?;
    encode(&sqzer, &source, &ready, &options)
}

/// Decode `bytes` once, to encode as often as a slider moves.
///
/// # Errors
/// Throws a `SqzerError`: `UnknownFormat` for bytes that are no image,
/// `DecoderUnavailable` for a format this build recognises and cannot read
/// (SVG, HEIC), which `decodeAny` hands to the browser, `TooLarge` over
/// `maxPixels`.
#[wasm_bindgen]
pub fn decode(bytes: &[u8], options: Option<Ts<DecodeOptions>>) -> Result<SqzerImage, SqzerError> {
    let options = DecodeOptions::parse(options)?;
    Ok(SqzerImage::new(sqzer(options.max_pixels).decode(bytes)?))
}

/// `decode`, with the browser's canvas for what this build cannot read:
/// SVG on the page, HEIC where the browser has a decoder for it.
///
/// The package's own decoder comes first. The browser gets only input it
/// has no decoder for, and returns 8 bits of sRGB. A vector image is
/// rasterised at its own size, or to fit inside `width` x `height` when
/// those are given; any other image is decoded at its own size.
///
/// In a worker there is no `<img>`, and Chrome, Firefox and Safari all
/// refuse an SVG blob in `createImageBitmap`, so an SVG fails there with
/// `DecoderUnavailable` and the browser's reason. Draw it on the page and
/// hand the pixels to `fromPixels`. HEIC on Safari works in both.
///
/// # Errors
/// Throws a `SqzerError`. When the browser cannot read the input either,
/// it is the `DecoderUnavailable` of `decode` with the browser's reason
/// added.
#[wasm_bindgen(js_name = decodeAny)]
pub async fn decode_any(
    bytes: Vec<u8>,
    options: Option<Ts<DecodeAnyOptions>>,
) -> Result<SqzerImage, SqzerError> {
    let options = DecodeAnyOptions::parse(options)?;
    let sqzer = sqzer(options.max_pixels);
    let (format, unavailable) = match sqzer.decode(&bytes) {
        Ok(decoded) => return Ok(SqzerImage::new(decoded)),
        Err(e) => match e {
            Error::DecoderUnavailable { format, .. } => (format, e),
            e => return Err(e.into()),
        },
    };
    let failed = |why: JsValue| {
        SqzerError::with_note(
            &unavailable,
            &format!(
                "the browser could not decode it either: {}",
                error::describe(&why)
            ),
        )
    };
    let source = open(&bytes, format.mime()).await.map_err(failed)?;
    // `rasterise` releases the source, drawn or not. An image refused
    // before it is released here: a bitmap holds decoded pixels the
    // garbage collector is slow to return.
    let (width, height) = match drawn_size(&source, format, &options, &sqzer) {
        Ok(size) => size,
        Err(e) => {
            close(&source);
            return Err(e.into());
        }
    };
    let rgba = rasterise(&source, width, height).map_err(failed)?;
    SqzerImage::from_rgba(&rgba, width, height, Some(format))
}

/// The size `decodeAny` draws `source` at, checked against the limit.
/// Only a vector image is drawn at another size than its own: pixels are
/// resized by `encode`, in linear light, not by the canvas.
fn drawn_size(
    source: &JsValue,
    format: Format,
    options: &DecodeAnyOptions,
    sqzer: &Sqzer,
) -> Result<(u32, u32), Error> {
    let (width, height) = match size(source)[..] {
        [w, h] if w > 0 && h > 0 => (w, h),
        _ => {
            return Err(Error::InvalidInput(format!(
                "the browser gives no size for this {format} image"
            )));
        }
    };
    let (width, height) = if format == Format::Svg {
        let fit = Resize {
            size: Size::Box {
                width: options.width,
                height: options.height,
            },
            enlarge: true,
            ..Resize::NONE
        };
        fit.check()?;
        fit.fit(width, height)
            .map_or((width, height), |geometry| geometry.output())
    } else {
        (width, height)
    };
    sqzer.decode_opts().check_pixels(width, height)?;
    Ok((width, height))
}

/// An image from pixels made elsewhere, a canvas say: `width` x `height`
/// RGBA, 8 bits, sRGB, alpha not premultiplied, which is what
/// `getImageData` returns. Fully opaque pixels make an image without an
/// alpha channel.
///
/// # Errors
/// Throws a `SqzerError`: `TooLarge` over `maxPixels`, as `decode` does,
/// and `InvalidInput` when `rgba` is not `width * height * 4` bytes.
#[wasm_bindgen(js_name = fromPixels)]
pub fn from_pixels(
    #[wasm_bindgen(unchecked_param_type = "Uint8Array | Uint8ClampedArray")] rgba: &[u8],
    width: u32,
    height: u32,
    options: Option<Ts<DecodeOptions>>,
) -> Result<SqzerImage, SqzerError> {
    let options = DecodeOptions::parse(options)?;
    sqzer(options.max_pixels)
        .decode_opts()
        .check_pixels(width, height)?;
    SqzerImage::from_rgba(rgba, width, height, None)
}

/// What this build decodes and encodes, and from which tier: one entry per
/// format `sqzer` knows, as `sqzer --list-codecs --json` prints them.
///
/// # Errors
/// None in practice: the listing always serialises.
#[wasm_bindgen]
pub fn codecs() -> Result<Vec<Ts<Codec>>, SqzerError> {
    output::codecs(&registry())
        .iter()
        .map(|codec| Ok(codec.into_ts()?))
        .collect()
}

/// A decoded image, from `decode`, `decodeAny` or `fromPixels`. Call
/// `free()` when done with it: its pixels live in the module's memory,
/// which the garbage collector does not see.
#[wasm_bindgen]
pub struct SqzerImage {
    decoded: Decoded,
    source: Source,
    /// The last transform and the options that made it. A slider that
    /// changes only the quality encodes from the same prepared, resized
    /// image, and gets it from here.
    ready: Cell<Option<(Stage, Ready)>>,
}

/// The options the stages before the encoder depend on.
#[derive(PartialEq)]
struct Stage {
    keep_icc: bool,
    keep_metadata: bool,
    resize: Resize,
    max_pixels: u64,
}

impl Stage {
    fn of(sqzer: &Sqzer) -> Self {
        Self {
            keep_icc: sqzer.params().keep_icc,
            keep_metadata: sqzer.params().keep_metadata,
            resize: sqzer.resize_bounds(),
            max_pixels: sqzer.decode_opts().max_pixels,
        }
    }
}

impl SqzerImage {
    fn new(decoded: Decoded) -> Self {
        Self {
            source: Source::of(&decoded),
            decoded,
            ready: Cell::new(None),
        }
    }

    /// Canvas pixels as an image that came from `format`, or from nowhere.
    fn from_rgba(
        rgba: &[u8],
        width: u32,
        height: u32,
        format: Option<Format>,
    ) -> Result<Self, SqzerError> {
        let expected = u64::from(width) * u64::from(height) * 4;
        if rgba.len() as u64 != expected {
            return Err(Error::InvalidInput(format!(
                "{width}x{height} RGBA pixels are {expected} bytes, got {}",
                rgba.len()
            ))
            .into());
        }
        let pixels = rgba.as_chunks::<4>().0;
        let image = if pixels.iter().all(|p| p[3] == u8::MAX) {
            let rgb = pixels.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            Image::from_u8(width, height, ColorType::Rgb, rgb)?
        } else {
            Image::from_u8(width, height, ColorType::Rgba, rgba.to_vec())?
        };
        let mut made = Self::new(Decoded {
            image,
            // `Decoded` wants a format. The records read `source`.
            info: FormatInfo {
                format: format.unwrap_or(Format::Png),
                animated: false,
            },
        });
        made.source.format = format;
        Ok(made)
    }
}

#[wasm_bindgen]
impl SqzerImage {
    /// Width in pixels, after EXIF orientation.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn width(&self) -> u32 {
        self.source.width
    }

    /// Height in pixels, after EXIF orientation.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn height(&self) -> u32 {
        self.source.height
    }

    /// The format it was decoded from, as `format` spells it. Undefined
    /// for an image made by `fromPixels`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn format(&self) -> Option<String> {
        self.source.format.map(|f| format_name(f).to_string())
    }

    /// Whether it has an alpha channel.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn alpha(&self) -> bool {
        self.source.alpha
    }

    /// Whether the input had more than one frame. The image is the first.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn animated(&self) -> bool {
        self.source.animated
    }

    /// Prepare, resize and encode the image. Takes what `optimize` takes
    /// and returns what it returns.
    ///
    /// # Errors
    /// Throws a `SqzerError`, whose `kind` says which failure it is.
    pub fn encode(&self, options: Option<Ts<Options>>) -> Result<Ts<Output>, SqzerError> {
        let options = Options::parse(options)?;
        let sqzer = options.sqzer()?;
        let stage = Stage::of(&sqzer);
        let ready = match self.ready.take() {
            Some((made, ready)) if made == stage => ready,
            _ => sqzer.transform(self.decoded.clone())?,
        };
        let out = encode(&sqzer, &self.source, &ready, &options);
        self.ready.set(Some((stage, ready)));
        out
    }
}

/// The encode stage, reporting each trial of the search to `onTrial`. An
/// exception `onTrial` throws cannot stop the search; the first one is
/// thrown again when the encode is over.
fn encode(
    sqzer: &Sqzer,
    source: &Source,
    ready: &Ready,
    options: &Options,
) -> Result<Ts<Output>, SqzerError> {
    let on_trial = options.on_trial();
    let mut thrown = None;
    let out = sqzer.encode_with(ready, |progress| {
        let (
            Some(on_trial),
            Progress::Trial {
                n,
                max,
                quality,
                score,
            },
        ) = (on_trial, progress)
        else {
            return;
        };
        let trial = TrialProgress {
            n,
            max,
            quality: tidy(quality),
            score: tidy(score),
        };
        let called = serde_wasm_bindgen::to_value(&trial)
            .map_err(JsValue::from)
            .and_then(|trial| on_trial.call1(&JsValue::NULL, &trial));
        if let Err(e) = called {
            thrown.get_or_insert(e);
        }
    })?;
    match thrown {
        Some(e) => Err(SqzerError::thrown(e)),
        None => Ok(Output::new(source, out, &sqzer.params().target).into_ts()?),
    }
}
