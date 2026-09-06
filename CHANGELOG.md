# Changelog

## Unreleased

### Features

* **core:** `Image` with validated construction, `u8`/`u16`/`f32` samples, an ICC slot, and `to_u8` / `without_alpha` adapters
* **core:** `Decoder`, `Encoder` and `Metric` traits with `DecoderCaps` and `EncoderCaps` descriptors
* **core:** `Registry` with probe, decode and per-format encoder dispatch; the last encoder registered for a format wins
* **core:** `Format` extension, MIME and `encoder_features` lookups
* **core:** `EncodeParams` gains `subsampling`, `resolved()` and `codec_opts()`
* **core:** `InvalidInput`, `InvalidParams` and `Unsupported` error variants
* **codecs:** PNG decoder and encoder over `png` (MIT OR Apache-2.0)
* **codecs:** JPEG encoder over `mozjpeg-rs` (BSD-3-Clause)
* **codecs:** `registry()` builds the registry from the active features, portable tier first
* **sqzer:** `Sqzer::run` decodes, picks an encoder and encodes, returning `Output`
* **sqzer:** a perceptual target is refused with `InvalidParams` until the SSIMULACRA2 search exists; pass `Target::Quality` or `Target::Lossless`
* **workspace:** scaffold, feature tiers, CI and the licence allow-list
