# Roadmap

What comes next for `sqzer`, roughly in order. `sqzer` is maintained by one person, so this is a direction, not a schedule, and the order can change when something more urgent turns up. Anything that changes the architecture gets an ADR in [`docs/adr`](docs/adr) before code.

Want to help with an item? Open an issue or a discussion first, see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## 0.1.1: decoder fixes

Two decoders drop information the rest of the pipeline expects to have.

- AVIF input ignores the ICC profile, the rotation (`irot`, `imir`) and the EXIF and XMP items, because `avif-parse` does not expose them. A wide-gamut or rotated AVIF comes out wrong even with `--keep-icc` or `--keep-metadata`. AVIF shares its container with HEIC, and the HEIC walker in `sqzer-codecs` already reads `irot`, `imir` and `colr` in safe Rust, so the plan is to reuse it; the EXIF and XMP items are new work.
- PNG input keeps its EXIF but does not apply the orientation stored in it.

## 0.2: in the browser

The "Squoosh replacement" is not complete without a page you can drop an image on (ADR-0001 item 10).

- `sqzer-wasm`: the portable tier as a WebAssembly package. The API and how it is published come in a new ADR first.
- A small drag-and-drop page on top of it.

> **Note**: The browser build inherits the portable tier's gaps. `rav1d` does not compile for `wasm32`, so AVIF input cannot be decoded there and AVIF output takes an explicit quality instead of the perceptual target. PNG goes through the plain `png` writer instead of `oxipng`, and lossy WebP and JPEG XL cannot be written at all.

## Resize

Everything past `--max-width` and `--max-height`: crop to a box, pad to a box, scale by a factor, choose the filter, and several widths from one decode for `srcset`. The interface is proposed in [ADR-0009](docs/adr/0009-resize-interface.md), in CSS `object-fit` terms instead of the `rimage` grammar ADR-0003 planned:

```sh
sqzer photo.jpg --width 400 --height 400 --fit cover --position top
sqzer photo.jpg --width 480,960,1600 -f avif,webp
```

## Coming from `rimage`

Palette reduction for PNG, `rimage`'s `--quantization`, as a codec option:

```text
-x png:colors=<N>
```

> **Note**: The quantiser needs a permissively licensed crate. `libimagequant` and its `imagequant` binding are GPL-3.0 and cannot be used.

## A full build in the browser

After 0.2, a second browser build over `sqzer-codecs-agpl` that fills the portable tier's gaps with imazen's pure-Rust codecs: lossy WebP (`zenwebp`), JPEG XL encoding, AVIF decoding, HEIC input, and PNG quantisation through `imagequant`. It would power the drag-and-drop page at `sqzer.dev` and make it a full Squoosh replacement.

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

- AGPL or GPL codecs in the library or the default binaries, including the imazen `zen*` crates, `jpegxl-rs` and `libimagequant`. The `agpl` tier stays reserved.
- `image`'s stock encoders as a backend.
- A fixed per-format quality as the default. The perceptual target is the product.
- Our own decoders, encoders, resamplers, colour transforms or metrics.
- Streaming decode. Every image is decoded fully in memory, behind the `--max-pixels` guard.
