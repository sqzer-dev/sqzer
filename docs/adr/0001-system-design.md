# ADR-0001: System design

**Status:** Proposed
**Date:** 2026-09-06
**Deciders:** Vlad (sole maintainer)
**Scope:** Clean redesign. The existing Rimage crate is prior art, not a constraint.

---

## 1. Context

Squoosh was the reference for "good defaults without thinking": drop an image in, pick a modern format, get a result that is close to the best a non-expert could tune by hand. It has no releases on GitHub and no batch mode, and the user experience is trapped in a browser tab. Nothing in the Rust ecosystem occupies that spot as a single tool: `oxipng`, `cavif`, `cjxl`, `cwebp` are all one-format, and `image`-based tools ship the crate's stock encoders, which are far from best in class for lossy output.

The goal is a library plus CLI that:

- accepts as many input formats as is realistic,
- writes the modern targets (AVIF, JPEG XL, WebP) and the classic ones (JPEG, PNG) with defaults that match or beat Squoosh's,
- runs anywhere Rust runs, including WASM, with a build that has zero C dependencies,
- stays maintainable by one person.

Two hard constraints came from the brief: a pure-Rust build must produce usable output for every major format, and the cross-compile matrix (Linux glibc and musl, macOS, Windows, WASM) must stay green.

### 1.1 What the ecosystem looks like today (September 2026)

This matters more than any architectural preference, because the codec landscape shifted hard in the first half of 2026. Imazen (the imageflow people) published a whole family of pure-Rust "zen*" codecs, but most of them are AGPL-3.0 or commercial. Meanwhile a few permissively licensed pure-Rust encoders reached parity with their C originals.

| Format | Pure Rust, permissive | Pure Rust, AGPL / commercial | C binding (permissive) | Notes |
|---|---|---|---|---|
| JPEG encode | `mozjpeg-rs` 0.9 (BSD-3): byte-identical to C mozjpeg in baseline/progressive, trellis output 0.05 to 0.8 % smaller and ~6 % faster | `zenjpeg` 0.8 (jpegli-derived: adaptive quant, XYB) | `mozjpeg` crate | mozjpeg-rs alone makes the pure-Rust JPEG story fully solved |
| JPEG decode | `zune-jpeg`, `jpeg-decoder` (both MIT/Apache) | | | Fine |
| PNG | `png` + `oxipng` (MIT), `zopfli` in Rust | | | Fully solved, no C needed |
| WebP encode | `image-webp` (MIT/Apache): **lossless only**, basic compressor | `zenwebp` 0.4: lossy + lossless, within 0.2 % of libwebp size, ~1.5x slower | `webp` / `libwebp-sys` | No permissive pure-Rust lossy WebP encoder exists |
| WebP decode | `image-webp` | `zenwebp` | | Fine |
| AVIF encode | `ravif` 0.13 (BSD-3, rav1e backend) | `zenravif` / `zenavif` | `libavif` + `libaom` via `libavif-sys` | rav1e is slower and slightly less efficient than libaom at equal quality, but good enough for the portable floor |
| AVIF decode | `rav1d` 1.1 (BSD-2, port of dav1d; ships assembly for x86/arm, has Rust fallback paths) | `rav1d-safe`, `zenavif` | `dav1d` binding | Need to verify rav1d builds on wasm32 with assembly disabled |
| JPEG XL encode | **none** | `jxl-encoder` 0.3 (lossless modular + lossy VarDCT), `zenjxl` | `jpegxl-rs` (libjxl, BSD-3) | This is the biggest gap. `jxl-oxide` and `jxl-rs` are decoders only |
| JPEG XL decode | `jxl-oxide` 0.12 (MIT/Apache), `jxl-rs` (BSD-3, WIP) | | | Fine |
| HEIC/HEIF decode | **none** | `heic` (imazen): I-frame HEVC, 49/49 conformance vectors, decodes most iPhone photos | `libheif-rs` (LGPL lib, GPL-tainted with x265) | Input only. Nobody wants HEIC output |
| SVG rasterize | `resvg` (MIT/Apache) | | | Input only |
| TIFF, GIF, BMP, TGA, ICO, QOI, PNM, DDS, EXR | `image` crate family (MIT/Apache) | `zentiff` | | Inputs, mostly solved |
| Quality metric | `fast-ssim2` 0.8 (BSD-2, SIMD, ~3.5x faster than scalar), `ssimulacra2` (rust-av), `dssim-core` | | | Fully solved |
| Shared codec traits | `zencodec` 0.1 (MIT/Apache) | | | Permissive even though most zen* codecs are not |

Take-aways that drive the design:

1. A permissive pure-Rust build can deliver best-in-class JPEG and PNG, decent AVIF, and lossless WebP. It cannot deliver lossy WebP or any JPEG XL encoding at all.
2. Everything the permissive build is missing exists in pure Rust under AGPL, or in C under BSD.
3. Licensing, not technology, is now the central architectural variable.

---

## 2. Requirements

**Functional**

- Decode: JPEG, PNG, WebP, AVIF, JXL, GIF (first frame and animation), TIFF, BMP, TGA, ICO, QOI, PNM, SVG, HEIC (opt-in), OpenEXR and HDR (opt-in).
- Encode: JPEG, PNG, WebP, AVIF, JXL. Lossless mode for each where the format supports it.
- Batch operation with sensible output naming, recursion, and parallelism.
- Resize, orientation normalization, metadata policy (strip vs keep), ICC handling.
- A defaults model that picks encoder parameters per image, not per format.
- Machine-readable output (JSON) for integrations.

**Non-functional**

- Pure-Rust build with `forbid(unsafe_code)` where practical and zero C toolchain requirement.
- Targets: `x86_64` and `aarch64` for Linux (glibc, musl), macOS, Windows (msvc); `wasm32-unknown-unknown` and `wasm32-wasip1`.
- Library API stable enough for Node, Python, and Bun bindings later.
- One maintainer: prefer fewer, well-chosen dependencies over maximal coverage.

---

## 3. Decisions

### D1. Crate topology: a workspace with a codec-free core

```
sqzer/
  crates/
    sqzer-core       pipeline, image model, codec traits, registry, presets. No codecs.
    sqzer-codecs     every backend behind a feature flag; implements core traits
    sqzer-metrics    SSIMULACRA2 / DSSIM wrappers, target-quality search
    sqzer            the public library facade (re-exports, builder API)
    sqzer-cli        the binary
    sqzer-wasm       wasm-bindgen surface, portable features only
  bindings/            (later) napi-rs, pyo3
```

`sqzer-core` defines three traits and a capability descriptor:

```rust
pub trait Decoder { fn probe(&self, bytes: &[u8]) -> Option<FormatInfo>; fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image>; }
pub trait Encoder { fn caps(&self) -> EncoderCaps; fn encode(&self, img: &Image, params: &EncodeParams) -> Result<Vec<u8>>; }
pub struct EncoderCaps { pub format: Format, pub lossy: bool, pub lossless: bool, pub alpha: bool, pub animation: bool, pub bit_depth: &'static [u8], pub hdr: bool, pub quality_range: RangeInclusive<f32>, pub speed_range: RangeInclusive<u8> }
```

The `EncodeParams` type is codec-agnostic: `Quality(f32 0..100)`, `Lossless`, `Effort(u8)`, `Subsampling`, plus an escape hatch `codec_specific: BTreeMap<&str, Value>`. Each backend maps the abstract quality to its own scale (a JPEG q75 and an AVIF q75 are not the same thing; the mapping tables are part of the defaults work in D4).

Why not adopt `zencodec` traits directly: they are MIT/Apache and well designed, but coupling the public API of this project to a 0.1.x crate maintained by a vendor whose codecs the project cannot ship by default is the wrong dependency direction. Write thin own traits; add a `zencodec` adapter in `sqzer-codecs` behind a feature if the zen* crates ever become shippable.

### D2. Codec backend tiering and the licensing line

This is the decision with the most consequences. Three options were considered.

#### Option A: Permissive-only (MIT/Apache core, BSD and MIT dependencies)

| Dimension | Assessment |
|---|---|
| Complexity | Low |
| Coverage | JPEG, PNG, AVIF (rav1e), lossless WebP in pure Rust. Lossy WebP and JXL only via C bindings |
| Legal risk | None |
| Adoption as a library | Best: any downstream can embed it |

**Pros:** clean story, no license audit for users, library can go into commercial products and other OSS tools.
**Cons:** the "pure Rust must work for every major format" constraint is violated for two formats: lossy WebP and JXL. The pure build must either omit them or fall back to something worse.

#### Option B: Embrace AGPL (project licensed AGPL-3.0, zen* crates by default)

| Dimension | Assessment |
|---|---|
| Complexity | Low |
| Coverage | Complete in pure Rust: zenwebp, jxl-encoder, zenavif, heic, zenjpeg |
| Legal risk | AGPL propagates to any binary, service, or library that links it |
| Adoption as a library | Poor: most companies ban AGPL dependencies outright |

**Pros:** the strongest possible pure-Rust story, fastest path to feature-complete.
**Cons:** kills the "nice-to-use library with integrations" goal. A Vite plugin or Next.js loader that pulls in AGPL code is a non-starter for most users. Also pins the project's fate to one vendor's licensing choices.

#### Option C: Permissive core, isolated AGPL backend crate, two published binary flavors

| Dimension | Assessment |
|---|---|
| Complexity | Medium |
| Coverage | Same as B for the "full" binary; same as A for the library and default binary |
| Legal risk | Contained, if discipline holds |
| Adoption as a library | Same as A |

Structure: `sqzer-codecs` stays permissive. A separate crate `sqzer-codecs-agpl` (published, but never a default dependency) implements the same traits over zen* crates. The CLI is released twice: `sqzer` (permissive) and `sqzer-full` (AGPL, statically includes everything). The library never references the AGPL crate; users opt in by adding it and registering its encoders.

**Pros:** honors both constraints where they can be honored, keeps the library clean, makes the licensing line visible rather than accidental.
**Cons:** doubles the release matrix, needs a CI check that the permissive crates never transitively pull in AGPL code (`cargo deny` with a license allow-list), and users will be confused about which binary to download.

#### Decision

**Option A for the first release, with the crate boundaries of Option C designed in from day one.** Concretely:

- Backend tiers are `portable` (pure Rust, permissive, default), `native` (C bindings, opt-in via features `native-webp`, `native-jxl`, `native-avif`, `native-heif`), and `agpl` (empty in v1, reserved).
- The portable tier honestly reports what it cannot do. Requesting JXL output in a portable build returns `Error::EncoderUnavailable { format, available_in: ["native-jxl"] }` rather than silently producing something else.
- `cargo deny` enforces the allow-list on every crate except `sqzer-codecs-agpl`.

The "pure Rust must work for every major format" constraint is therefore met for JPEG, PNG, and AVIF, met for lossless WebP, and consciously not met for lossy WebP and JXL in v1. Two things can change that later: imazen relicensing (they have done it before: zenwebp 0.3 was permissive, 0.4 is AGPL, so the reverse is possible but not to be assumed), or a permissive pure-Rust JXL encoder emerging (libjxl's `jxl-rs` is decoder-only and there is no public encoder roadmap). Watch both; do not wait on either.

### D3. Pipeline model

A linear, explicit pipeline with one intermediate image type:

```
bytes -> probe -> decode -> Image -> [orient, resize, color-manage] -> Image -> encode -> bytes
                                                                    \-> metric loop (D4)
```

`Image` is a planar-agnostic container: `u8`, `u16`, or `f32` samples; `Rgb`, `Rgba`, `Gray`, `GrayA`; an optional ICC profile; optional EXIF orientation already applied; optional animation frames with per-frame delay. Color management uses `qcms` or `lcms2` (feature-gated: `qcms` is Mozilla's pure-Rust CMS, MIT, but its last crates.io release is from early 2024 and needs a maintenance check; `lcms2` is a C binding). Everything is converted to sRGB before encode unless `--keep-icc` is set, because encoder quality tables are calibrated on sRGB and because that is what Squoosh did.

Resizing uses `fast_image_resize` (MIT/Apache, SIMD, pure Rust). Downscale before encode, never after decode-for-metrics, so the metric measures what the user will see.

Parallelism is per-file via `rayon` in the CLI. Encoders are single-threaded by default and given a thread budget explicitly; rav1e and libaom can each eat every core and fighting over them with rayon halves throughput. On WASM, rayon is compiled out and the pipeline runs sequentially.

Streaming is out of scope. Every image is decoded fully in memory. Images too large to fit produce an error with the estimated requirement, not an OOM. A `--max-pixels` guard defaults to 268 megapixels (16k × 16k) as decompression-bomb protection.

### D4. Defaults policy: perceptual target, not a quality slider

Squoosh's headline value was that its slider defaults (q75 for MozJPEG and WebP, cq-level 33 for AVIF, q75 for JXL, and "effort" set to the highest value that still felt interactive) were tuned by people who understood the codecs. A fixed number per format is still the wrong primitive: the same q75 is overkill on a flat screenshot and visibly lossy on a noisy photo.

Three modes, in order of precedence:

1. **Target mode (default).** The user states a perceptual target: `--target 70` on the SSIMULACRA2 scale (100 is identical; 70 is "high quality, no visible artefacts on a normal display" in the metric's own calibration; 50 is "medium, artefacts visible on close inspection"). The encoder's quality parameter is bisected until the score is within a tolerance of the target, capped at 6 iterations. `fast-ssim2` makes a 12 MP score cost tens of milliseconds, so the loop is cheaper than one slow AVIF encode. Default target is 70 for the `web` preset.
2. **Preset mode.** `--preset web|thumbnail|archive|lossless`. Each preset is a target plus format-specific knobs (chroma subsampling, effort, progressive). `lossless` bypasses the metric loop.
3. **Explicit mode.** `--quality 82 --effort 6` and codec-specific overrides via `--codec-opt avif:tune=ssim`. Any explicit quality disables the search.

The per-format quality tables that seed the bisection (so the first guess is usually within one step of the answer) are generated offline by `codec-eval` (imazen, permissive) against a corpus of photographic, illustration, and UI screenshots, and committed as data. Regenerating them is a maintenance task documented in the repo, not a build step.

Why SSIMULACRA2 and not DSSIM or Butteraugli: it has the best published correlation with human opinion (87 to 98 % per codec-eval's summary), it is the metric libjxl and the AV1 image community tune against, and the fastest permissive implementation is already in Rust.

Failure mode to design for explicitly: an image on which no quality value reaches the target (tiny images, extreme content). The search terminates at the ceiling and reports it; the CLI prints a warning in verbose mode and never errors.

### D5. CLI surface

Principles: the common case is one word, batch is the default shape, output never overwrites input unless asked.

```
sqzer photo.jpg                          # -> photo.avif next to it (default target format for photos)
sqzer photo.jpg -f webp,avif,jxl         # one input, three outputs
sqzer ./assets -r -f avif -o ./dist      # recurse, mirror the tree into dist
sqzer *.png --preset lossless -f webp    # lossless conversion
sqzer in.png --target 60 --max-width 1600
sqzer in.png --json                      # machine output: sizes, scores, chosen params
sqzer --list-codecs                      # shows portable/native availability in this build
```

Content-aware default target format: photographic input defaults to AVIF, graphics with hard edges or few colours default to lossless WebP or PNG, and animated input defaults to animated WebP or AVIF. Detection is a cheap heuristic (unique-colour count, edge density) in `sqzer-core`, not machine learning.

Exit codes: 0 success, 1 partial failure in batch (individual errors listed), 2 argument error, 3 nothing could be done (no encoder available). Output naming is `{stem}.{ext}` by default with `--suffix` and `--template "{stem}-{width}w.{ext}"` for responsive-image workflows.

### D6. Build and target matrix

| Target | Portable build | Native build | CI |
|---|---|---|---|
| x86_64 / aarch64 linux-gnu | yes | yes | every push |
| x86_64 / aarch64 linux-musl | yes | yes (static) | every push |
| x86_64 / aarch64 apple-darwin | yes | yes | every push |
| x86_64 windows-msvc | yes | yes (vcpkg) | every push |
| wasm32-unknown-unknown | yes, no threads | no | every push |
| wasm32-wasip1 | yes | no | weekly |

Release binaries: portable for every target, native for the six desktop targets. `cargo-dist` handles the matrix. `cargo deny` runs the license allow-list, and a dedicated CI job builds `sqzer-wasm` with `--no-default-features --features portable` so an accidental C dependency fails fast.

Known risk: `rav1d` bundles assembly. Its Rust fallback path must be verified to build on wasm32 with the `asm` feature disabled; if it does not, AVIF decoding on WASM becomes a documented gap rather than a blocker, because AVIF is an output format first.

### D7. Input coverage and metadata policy

Decoder registration is capability-driven, so adding a format is one file. v1 decoders in the portable tier: `zune-jpeg`, `png`, `image-webp`, `rav1d` + `avif-parse` (MPL-2.0, file-level copyleft only, so it goes on the allow-list; write an own ISOBMFF parser only if that ever becomes a problem), `jxl-oxide`, `gif`, `tiff`, `image` for BMP/TGA/ICO/QOI/PNM/DDS, `resvg` for SVG, `exr` for OpenEXR. Native tier adds `libheif` for HEIC. Camera RAW is out of scope: `rawloader` is LGPL and the demosaic quality question is a separate project.

Metadata defaults follow Squoosh: strip everything by default, keep ICC when it is not sRGB (converting instead is the default), `--keep-metadata` to retain EXIF and XMP, and always apply then remove EXIF orientation. Rationale: most people running an optimizer are publishing to the web, and shipping GPS coordinates in EXIF is the most common accidental leak.

### D8. Library API for integrations

One builder, sync, with an async wrapper later if a binding needs it:

```rust
let out = Scrunch::new()
    .input_bytes(&bytes)
    .format(Format::Avif)
    .target(Target::Ssimulacra2(70.0))
    .max_width(1600)
    .run()?;
out.bytes, out.report.score, out.report.chosen_quality
```

Bindings come after the CLI stabilizes: `napi-rs` first (Vite, Next, Astro plugins are where Squoosh's audience went), `pyo3` second. The WASM package is the portable tier only, exposed through `wasm-bindgen` with the same builder.

---

## 4. Trade-off analysis

**Pure-Rust floor vs best output.** The portable tier is never the best encoder for AVIF (libaom beats rav1e) and cannot do lossy WebP or JXL at all. Accepting this keeps the library permissive and the WASM build honest. The mitigation is to make the native tier a first-class release artifact, not a hidden feature.

**Perceptual search vs speed.** Target mode multiplies encode cost by up to six for slow codecs. Mitigations: seeded first guess from calibration tables, a `--fast` flag that skips the loop, and running the search on a downscaled proxy for images above a size threshold, then verifying once at full size.

**Own traits vs `zencodec`.** Slight duplication in exchange for owning the public API. Revisit if `zencodec` reaches 1.0 and its codecs become shippable.

**Two tiers now vs three tiers later.** Reserving the `agpl` tier without populating it costs nothing but a crate name and a `cargo deny` rule, and avoids a rewrite if the licensing picture changes.

---

## 5. Consequences

What becomes easier:

- Adding a codec is implementing two traits and a capability struct; the CLI, metric loop, presets, and JSON output need no changes.
- Explaining the project: "Squoosh's defaults as a CLI and library, pure Rust by default, C codecs when you want the last few percent."
- Embedding: the permissive library is safe in any build pipeline.

What becomes harder:

- Two build flavors means two sets of bugs. Native features must have CI parity, not just "it compiles".
- The perceptual-target model needs a calibration corpus and a regeneration workflow that one person has to keep alive.
- Explaining to users why the WASM build cannot write JXL.

What to revisit:

- The AGPL tier decision, every six months or when imazen changes licenses.
- The default target score once real feedback arrives; 70 is a calibrated guess, not a measured preference.
- Whether `rav1d` on wasm32 is viable, before promising AVIF decode in the browser package.

---

## 6. Action items

1. [x] Scaffold the workspace (D1) with `cargo deny` license allow-list and the empty `sqzer-codecs-agpl` crate.
2. [x] Implement `Image`, the three traits, and the registry with `png` + `mozjpeg-rs` as the first pair to prove the shape.
3. [x] Add portable decoders: `zune-jpeg`, `image-webp`, `jxl-oxide`, `rav1d` (verify wasm32 build in the same PR).
4. [x] Add portable encoders: `oxipng`, `ravif`, `image-webp` lossless. (`oxipng` is desktop only, see ADR-0002.)
5. [ ] Add `sqzer-metrics` with `fast-ssim2` and the bisection loop; write the failure-mode tests (tiny image, flat image, noise).
6. [ ] Build the calibration harness on `codec-eval`; commit seed tables for JPEG, AVIF, WebP.
7. [ ] CLI with the six command shapes from D5, JSON output, exit codes.
8. [ ] Native tier: `libwebp`, `jpegxl-rs`, `libavif` + `libaom`, `libheif`; CI jobs for each on the six desktop targets.
9. [ ] `cargo-dist` release matrix, portable + native artifacts.
10. [ ] `sqzer-wasm` package and a minimal browser demo (the "Squoosh replacement" story is not complete without a drag-and-drop page, even if it is a 200-line HTML file).
11. [x] Name: `sqzer`. crates.io and npm were free on 2026-09-06.

---

## Sources

- [mozjpeg-rs on lib.rs](https://lib.rs/crates/mozjpeg-rs), [crates.io metadata](https://crates.io/api/v1/crates/mozjpeg-rs)
- [zenjpeg on lib.rs](https://lib.rs/crates/zenjpeg), [crates.io metadata](https://crates.io/api/v1/crates/zenjpeg)
- [zenwebp on lib.rs](https://lib.rs/crates/zenwebp), [crates.io metadata](https://crates.io/api/v1/crates/zenwebp)
- [image-webp on GitHub](https://github.com/image-rs/image-webp)
- [jxl-encoder on docs.rs](https://docs.rs/jxl-encoder)
- [jxl-oxide crates.io metadata](https://crates.io/api/v1/crates/jxl-oxide)
- [jxl-rs on GitHub](https://github.com/libjxl/jxl-rs)
- [zenavif on docs.rs](https://docs.rs/zenavif/latest/zenavif/)
- [ravif crates.io metadata](https://crates.io/api/v1/crates/ravif)
- [rav1d crates.io metadata](https://crates.io/api/v1/crates/rav1d)
- [rav1d-safe crates.io metadata](https://crates.io/api/v1/crates/rav1d-safe)
- [imazen/heic on GitHub](https://github.com/imazen/heic)
- [zencodec crates.io metadata](https://crates.io/api/v1/crates/zencodec)
- [fast-ssim2 on GitHub](https://github.com/imazen/fast-ssim2)
- [codec-eval on lib.rs](https://lib.rs/crates/codec-eval)
- [rust-av/ssimulacra2 on GitHub](https://github.com/rust-av/ssimulacra2)
- [oxipng on GitHub](https://github.com/oxipng/oxipng)
- [Squoosh releases page](https://github.com/GoogleChromeLabs/squoosh/releases)
