# Roadmap

What comes next for `sqzer`, roughly in order. `sqzer` is maintained by one person, so this is a direction, not a schedule, and the order can change when something more urgent turns up. Anything that changes the architecture gets an ADR in [`docs/adr`](docs/adr) before code.

Want to help with an item? Open an issue or a discussion first, see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Coming from `rimage`

Palette reduction for PNG, `rimage`'s `--quantization`, as a codec option:

```text
-x png:colors=<N>
```

> **Note**: The quantiser needs a permissively licensed crate. `libimagequant` and its `imagequant` binding are GPL-3.0 and cannot be used.

## A full build in the browser

The page at [sqzer.dev](https://sqzer.dev) runs the portable tier, so it inherits that tier's licence gaps: lossy WebP and JPEG XL cannot be written ([`docs/adr/0011-browser-build.md`](docs/adr/0011-browser-build.md)). Next is a second browser build over `sqzer-codecs-agpl` that fills those gaps with imazen's pure-Rust codecs: lossy WebP (`zenwebp`), JPEG XL encoding, HEIC input, and PNG quantisation through `imagequant`. It would power the page and make it a full Squoosh replacement.

> **Note**: That build is AGPL-3.0, and so is anything that bundles it. It ships as its own package, never as the default `sqzer` package or in the library, which stay permissive. Whether those crates build for `wasm32` is not verified yet; that check and a new ADR come first.

## Formats

- Animation. GIF, WebP and AVIF input keep their first frame today. Carrying every frame through the pipeline and writing animated WebP and AVIF needs frames on `Image` and an ADR.
- Radiance HDR input, opt-in, as ADR-0001 scopes it.
- JPEG XL encoding in the portable tier: measure `jixel` against `gamut-jxl` on the calibration corpus, and propose it in a new ADR if it is close (ADR-0004 item 5).
- Lossy WebP in the portable tier, once a permissively licensed pure-Rust encoder exists. None does as of September 2026.

## Release binaries

- The musl archive gains JPEG XL encoding and jpegli if `cargo-zigbuild` can build the C++ backends there (ADR-0006 item 7).
- CI for the native combinations it does not cover yet: jpegli on Windows and aarch64 Linux, the C++ backends on musl (ADR-0004 item 4).

## Later

- Node and Python bindings (`napi-rs`, `pyo3`) once the library API has settled.

## Not planned

These are decisions, not gaps. The reasoning is in ADR-0001.

- AGPL or GPL codecs in the library or the default binaries, including the imazen `zen*` crates, `jpegxl-rs` and `libimagequant`. They go only into the separate AGPL build above.
- `image`'s stock encoders as a backend.
- A fixed per-format quality as the default. The perceptual target is the product.
- Our own decoders, encoders, resamplers, colour transforms or metrics.
- Streaming decode. Every image is decoded fully in memory, behind the `--max-pixels` guard.
