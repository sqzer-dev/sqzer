# sqzer

Multi-format image optimizer with best-in-class defaults. A library and a CLI, pure Rust by default, C codecs when you want the last few percent.

> **Note**: Private, pre-alpha. The portable build decodes JPEG, PNG, WebP, AVIF and JPEG XL and writes JPEG, PNG, lossless WebP and AVIF; the native build adds lossy WebP, JPEG XL, `libaom` AVIF, jpegli JPEG and HEIC input. Resize and colour management are still to come. The design is in [`docs/adr/0001-system-design.md`](docs/adr/0001-system-design.md), the command line in [`docs/adr/0003-cli-interface.md`](docs/adr/0003-cli-interface.md), the native backends in [`docs/adr/0004-native-tier.md`](docs/adr/0004-native-tier.md).

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

The default mode is a perceptual target, not a quality slider. `sqzer` searches encoder quality until the output hits a SSIMULACRA2 score (70 by default), so a flat screenshot and a noisy photo get different settings for the same visible result. The search starts from a seed table calibrated per encoder on a corpus of photos, graphics and screenshots, so the first encode is usually close: on a held-out split the search takes two to four encodes instead of four to six, at the same quality and size.

## Command line

One flat command. Flags can go anywhere, `-q` means quality everywhere, and codec-specific knobs go through one flag instead of one flag per codec. `sqzer -h` shows the flags most runs need, `sqzer --help` shows all of them.

```sh
# the target: a SSIMULACRA2 score (default 70), an explicit quality, or lossless
sqzer photo.jpg -t 60
sqzer photo.jpg -q 82 -e 8
sqzer photo.jpg --lossless -f png
sqzer photo.jpg --preset thumbnail          # web (70), thumbnail (60), archive (85), lossless
sqzer photo.jpg --fast                      # one encode at the calibrated seed quality, no search

# codec-specific options, checked against the backend before anything runs
sqzer photo.jpg -f jpeg -x jpeg:progressive=false
sqzer --list-codecs -v                      # every backend's keys and defaults

# where outputs go
sqzer photo.jpg -o out.avif                 # a file, when there is one input and one format
sqzer *.jpg -o dist                         # a directory otherwise
sqzer *.jpg --suffix -min                   # photo-min.avif next to photo.jpg
sqzer *.jpg --template "{stem}-{width}w.{ext}"
sqzer *.jpg --in-place --backup             # write over the input, keep photo@backup.jpg
sqzer ./assets -r -o dist --exclude "**/raw/*"

# inputs from a list or a pipe
find . -name '*.png' -print0 | sqzer --files-from - -0 -f webp --lossless
curl -s https://example.com/a.png | sqzer - -f png --lossless > a.png

# machine output and planning
sqzer *.jpg --json                          # one JSON Lines object per output, nothing else on stdout
sqzer *.jpg -n --json                       # dimensions, alpha, format and planned outputs, no encode
```

Defaults that differ from most optimisers: the output never overwrites the input unless `--in-place` is given, an output larger than its input is not written unless `--force` is, and metadata is stripped with the ICC profile converted to sRGB (`--keep-icc` keeps it). Every such "nothing happened" prints one line saying why and which flag changes it.

Exit codes, from ADR-0001:

```text
0   every input produced every requested output (skipped-as-larger counts as success)
1   at least one input failed; the failures are on stderr and in --json
2   argument error, including --target with --quality and an unknown --codec-opt key
3   nothing could be done: no input matched, or no encoder for the format in this build
```

Paths: an argument that exists on disk is taken literally and never parsed as a glob. One that does not exist and contains `*`, `?` or `[` is expanded by `sqzer` itself, case-insensitively, so `*.png` works in `cmd.exe` and finds `.PNG`. The output stem is everything before the last dot, so `a.b.c.jpg` becomes `a.b.c.avif`.

Memory: `-j` sets how many files are in flight and defaults to the CPU count, but the decoder is also held to a pixel budget of `--max-pixels` times jobs over four, so a folder of huge images is processed a few at a time instead of all at once. Encoders run single-threaded; the parallelism is across files.

### Coming from `rimage`

`rimage` had one subcommand per codec and local flags under each. `sqzer` has one flat command; the codec is `-f`. A `rimage` codec name as the first argument prints the equivalent `sqzer` line and exits 2.

```text
rimage mozjpeg -q 75 in.jpg          sqzer -f jpeg -q 75 in.jpg
rimage oxipng in.png                 sqzer -f png in.png
rimage webp -q 80 in.png             sqzer -f webp -q 80 in.png    (lossy WebP needs the native tier)
rimage avif in.jpg                   sqzer -f avif in.jpg
-d <dir>                             -o <dir>
-s <suffix>                          --suffix <suffix>
-t <threads>                         -j <jobs>        (-t is now --target)
--quantization / --dithering         --codec-opt png:colors=  (once a quantiser lands)
--resize <spec>                      not yet; resize ships with the pipeline stage
--backup                             --backup, unchanged, with --in-place
```

## Formats

Decode: JPEG, PNG, WebP, AVIF, JPEG XL, GIF, TIFF, BMP, TGA, ICO, QOI, PNM, SVG. HEIC and OpenEXR behind features.

Encode: JPEG, PNG, WebP, AVIF, JPEG XL.

Backends come in tiers, mirrored by Cargo features:

```
portable   pure Rust, permissive licences, builds on wasm32. Always on.
           JPEG (mozjpeg-rs / zune-jpeg), PNG (oxipng), AVIF (ravif / re_rav1d),
           WebP (image-webp, lossless write), JXL decode (jxl-oxide).
native     C bindings, opt-in, one feature per library, `native` for all five.
           native-webp    libwebp, lossy and lossless WebP (webpx)
           native-jxl     libjxl, JPEG XL encoding (gamut-jxl)
           native-avif    libavif + libaom, AVIF encoding (libavif)
           native-heif    libheif, HEIC decoding, system library (libheif-rs)
           native-jpegli  jpegli, JPEG encoding (jpegli)
           A native encoder takes its format over from the portable one.
agpl       reserved. Never a default dependency, never in the library.
```

> **Note**: The portable tier cannot write lossy WebP or JPEG XL. No permissive pure-Rust encoder exists for either as of September 2026 (`jixel` is a candidate for JPEG XL, unmeasured). Requesting one in a portable build returns `EncoderUnavailable` with the feature that would provide it, it never silently falls back.

> **Note**: Every native feature vendors and builds its C library from source (`cc` or cmake; nasm on x86; a C++ compiler for `native-jxl` and `native-jpegli`), except `native-heif`, which links the system `libheif` (LGPL-3.0, >= 1.17, with an HEVC decoder) through `pkg-config`, or vcpkg on Windows. [`docs/adr/0004-native-tier.md`](docs/adr/0004-native-tier.md) has the crate choices and the licence facts, and which targets CI covers: all five on Linux glibc and macOS, three on Windows (no `libheif` from vcpkg yet, and jpegli cannot share a cmake generator with libjxl there), two on musl (no C++ toolchain there yet).

> **Note**: AVIF decoding is desktop only. `rav1d` does not compile for `wasm32`, so the WASM build recognises AVIF input but has no decoder for it. AVIF encoding builds everywhere, but a perceptual target needs the output decoded to score it, so on `wasm32` AVIF takes an explicit quality only and the default output format there is JPEG.

> **Note**: `oxipng` is the one portable backend that is not pure Rust: its DEFLATE step is `libdeflate`, a vendored C library compiled by `cc` with no system package to install. It is compiled out on `wasm32`, where the plain `png` writer takes its place, so the WASM build stays C-free. [`docs/adr/0002-libdeflate-in-the-portable-tier.md`](docs/adr/0002-libdeflate-in-the-portable-tier.md) has the reasoning.

## Layout

```
crates/sqzer-core      image model, codec traits, params, errors. No codecs.
crates/sqzer-codecs    every backend behind a feature flag
crates/sqzer-metrics   SSIMULACRA2 and the target-quality search
crates/sqzer           library facade, the thing you depend on
crates/sqzer-cli       the binary, `sqzer`
crates/sqzer-wasm      browser build, portable tier only
docs/adr               design decisions
```

## Development

```sh
# everything, portable tier
cargo build --workspace

# native tier: cmake, a C++ compiler and nasm on PATH, libheif-dev installed
# (`brew install libheif` on macOS); cmake 4 needs CMAKE_POLICY_VERSION_MINIMUM=3.5
# for the vendored libjpeg-turbo in jpegli's tree. Or one backend at a time,
# for example --features native-webp,native-avif on a box without a C++ compiler
cargo build -p sqzer-cli --features native
cargo test -p sqzer-codecs -p sqzer -p sqzer-cli --features sqzer-codecs/native,sqzer/native,sqzer-cli/native

# prove the portable tier stays C-free
cargo build -p sqzer-wasm --target wasm32-unknown-unknown

# licence allow-list, runs in CI
cargo deny check

cargo test --workspace
cargo clippy --workspace --all-targets
```

The seed tables that start the target search live in `crates/sqzer-metrics/src/seeds/tables.rs` and are regenerated by hand, not at build time:

```sh
# downloads the corpus on first use, sweeps every lossy encoder, rewrites the tables
cd tools/calibrate && cargo run --release -- sweep

# then measures the new tables on a held-out split
cargo run --release -- verify
```

`tools/calibrate` is its own package, outside the workspace, because `codec-eval` pulls in an AGPL dependency that must not reach anything that ships. [`tools/calibrate/README.md`](tools/calibrate/README.md) has the details.

## Licence

MIT or Apache-2.0, at your option.
