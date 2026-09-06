# sqzer

Multi-format image optimizer with best-in-class defaults. A library and a CLI, pure Rust by default, C codecs when you want the last few percent.

> **Note**: Private, pre-alpha. The library decodes JPEG, PNG, WebP, AVIF and JPEG XL and writes PNG and JPEG; everything else below is the plan. The design is in [`docs/adr/0001-system-design.md`](docs/adr/0001-system-design.md).

## What it is for

Squoosh had one thing nobody else had: defaults tuned by people who understood the codecs. Drop an image in, pick AVIF, get something close to the best a non-expert could tune by hand. It has no releases, no batch mode and lives in a browser tab.

`sqzer` is that, as a tool you can script:

```sh
# one photo, default target format (AVIF for photos), written next to the input
sqzer photo.jpg

# one input, three outputs
sqzer photo.jpg -f webp,avif,jxl

# recurse a folder, mirror the tree into ./dist
sqzer ./assets -r -f avif -o ./dist

# lossless conversion
sqzer *.png --preset lossless -f webp

# perceptual target instead of a quality number, plus a resize
sqzer in.png --target 60 --max-width 1600

# machine output for integrations: sizes, scores, chosen params
sqzer in.png --json
```

The default mode is a perceptual target, not a quality slider. `sqzer` searches encoder quality until the output hits a SSIMULACRA2 score (70 by default), so a flat screenshot and a noisy photo get different settings for the same visible result.

## Formats

Decode: JPEG, PNG, WebP, AVIF, JPEG XL, GIF, TIFF, BMP, TGA, ICO, QOI, PNM, SVG. HEIC and OpenEXR behind features.

Encode: JPEG, PNG, WebP, AVIF, JPEG XL.

Backends come in tiers, mirrored by Cargo features:

```
portable   pure Rust, permissive licences, builds on wasm32. Always on.
           JPEG (mozjpeg-rs / zune-jpeg), PNG (oxipng), AVIF (ravif / re_rav1d),
           WebP (image-webp, lossless write), JXL decode (jxl-oxide).
native     C bindings, opt-in. libwebp, libjxl, libavif + libaom, libheif.
agpl       reserved. Never a default dependency, never in the library.
```

> **Note**: The portable tier cannot write lossy WebP or JPEG XL. No permissive pure-Rust encoder exists for either as of September 2026. Requesting one in a portable build returns `EncoderUnavailable` with the feature that would provide it, it never silently falls back.

> **Note**: AVIF decoding is desktop only. `rav1d` does not compile for `wasm32`, so the WASM build recognises AVIF input but has no decoder for it.

## Layout

```
crates/sqzer-core      image model, codec traits, params, errors. No codecs.
crates/sqzer-codecs    every backend behind a feature flag
crates/sqzer-metrics   SSIMULACRA2 and the target-quality search
crates/sqzer           library facade, the thing you depend on
crates/sqzer-cli       the binary
crates/sqzer-wasm      browser build, portable tier only
docs/adr               design decisions
```

## Development

```sh
# everything, portable tier
cargo build --workspace

# native tier (needs the C libraries on PATH / vcpkg)
cargo build -p sqzer-cli --features native

# prove the portable tier stays C-free
cargo build -p sqzer-wasm --target wasm32-unknown-unknown

# licence allow-list, runs in CI
cargo deny check

cargo test --workspace
cargo clippy --workspace --all-targets
```

## Licence

MIT or Apache-2.0, at your option.
