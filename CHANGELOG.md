# Changelog

## Unreleased

### Features

* **core:** `Image` with validated construction, `u8`/`u16`/`f32` samples, an ICC slot, and `to_u8` / `without_alpha` adapters
* **core:** `Decoder`, `Encoder` and `Metric` traits with `DecoderCaps` and `EncoderCaps` descriptors
* **core:** `Registry` with probe, decode and per-format encoder dispatch; the last encoder registered for a format wins
* **core:** `Format` extension, MIME and `encoder_features` lookups
* **core:** `EncodeParams` gains `subsampling`, `resolved()` and `codec_opts()`
* **core:** `InvalidInput`, `InvalidParams` and `Unsupported` error variants
* **core:** `Orientation` and `Image::apply_orientation`, the shared EXIF orientation transform decoders apply
* **codecs:** PNG decoder and encoder over `png` (MIT OR Apache-2.0)
* **codecs:** JPEG encoder over `mozjpeg-rs` (BSD-3-Clause)
* **codecs:** JPEG decoder over `zune-jpeg` (MIT OR Apache-2.0 OR Zlib): grayscale stays gray, EXIF orientation applied, ICC kept
* **codecs:** WebP decoder over `image-webp` (MIT OR Apache-2.0): lossy, lossless and the first frame of animations, EXIF orientation applied, ICC kept
* **codecs:** JPEG XL decoder over `jxl-oxide` (MIT OR Apache-2.0): 8-bit sources to `u8`, deeper ones to `u16`; a file with its own ICC profile keeps it, everything else is rendered to sRGB
* **codecs:** AVIF decoder over `avif-parse` (MPL-2.0), `re_rav1d` (BSD-2-Clause) and `yuv` (BSD-3-Clause OR Apache-2.0): 8, 10 and 12-bit, monochrome and alpha items. Desktop targets only, `rav1d` does not build for wasm32
* **codecs:** PNG encoder over `oxipng` (MIT): filter and colour-type search, `zopfli` at effort 10, ICC kept. Desktop targets only; the wasm32 build keeps the plain `png` writer because `oxipng` depends on the C library `libdeflate` (ADR-0002)
* **codecs:** WebP lossless encoder over `image-webp` (MIT OR Apache-2.0): RGB, RGBA, gray widened to RGB, ICC kept, `webp:predictor` option
* **codecs:** AVIF encoder over `ravif` (BSD-3-Clause) and `rav1e` (BSD-2-Clause): 8-bit input, 4:4:4, alpha item, 10-bit payload by default, `avif:alpha_quality`, `avif:bit_depth` and `avif:color_model` options. Builds on every target including wasm32. Refuses lossless, chroma subsampling and images carrying an ICC profile rather than approximating them
* **codecs:** codec option parsing is shared across backends; error messages name the option as `codec:key`
* **sqzer:** a lossy target with no explicit format now defaults to AVIF when the build has an AVIF encoder; a lossless target still defaults to PNG
* **codecs:** `DecodeOpts::apply_orientation` is honoured by the JPEG and WebP decoders; JPEG XL applies its own orientation field unconditionally
* **codecs:** `registry()` builds the registry from the active features, portable tier first
* **sqzer:** `Sqzer::run` decodes, picks an encoder and encodes, returning `Output`
* **sqzer:** a perceptual target is refused with `InvalidParams` until the SSIMULACRA2 search exists; pass `Target::Quality` or `Target::Lossless`
* **workspace:** scaffold, feature tiers, CI and the licence allow-list
