# ADR-0004: Native tier backends

**Status:** Accepted
**Date:** 2026-09-10
**Deciders:** Vlad (sole maintainer)
**Scope:** Which crates back the `native-*` features of `sqzer-codecs`, what each one is allowed to do, how the C code gets built, and what CI covers. Closes ADR-0001 action item 8. Nothing here changes the tier rules of ADR-0001 D2: the portable tier stays the default and the native tier stays opt-in, desktop only, and permissively licensed.

---

## 1. Context

ADR-0001 named the native tier by library: `libwebp`, `jpegxl-rs`, `libavif` + `libaom`, `libheif`. Picking the Rust bindings turned out to be the actual decision, because the obvious crate is the wrong one twice over:

- `jpegxl-rs` and `jpegxl-sys` are GPL-3.0-or-later on crates.io. libjxl itself is BSD-3-Clause; the binding is not. A GPL crate cannot go on the `deny.toml` allow-list, so the ADR-0001 plan for JPEG XL was never buildable as written.
- The `webp` crate, which the calibration harness already used, wraps `libwebp-sys` 0.9 (libwebp 1.2) and has no way to write an `ICCP` chunk, so a lossy WebP would lose the profile the portable lossless writer keeps.

Two more things came up while looking:

- jpegli, libjxl's perceptually tuned JPEG encoder, has a BSD-3-Clause binding (`jpegli` over `jpegli-sys`, a mozjpeg-style wrapper) and beats mozjpeg on SSIMULACRA2 at the same size for most photographic content. It was not in the ADR-0001 list. It fits the tier's purpose, "C codecs when you want the last few percent", better than anything else in it.
- `jixel` 0.2 (BSD-3-Clause OR Apache-2.0) is a pure-Rust lossy JPEG XL encoder ported from libjxl-tiny. If it holds up, it retires the "no permissive pure-Rust JXL encoder exists" line in ADR-0001 section 1.1. It needs Rust 1.94 and a quality comparison against libjxl before it can be the portable tier's JXL writer. Out of scope here; see the action items.

The workspace forbids `unsafe`, so every backend needs a safe wrapper crate; a bare `-sys` crate is not usable from `sqzer-codecs`.

## 2. Decision

Five features, each one crate deep from `sqzer-codecs`'s point of view. All are single-threaded per encode (ADR-0001 D3) and register after the portable tier so they take their format over.

```
native-webp    webpx 0.4 (MIT OR Apache-2.0) over libwebp-sys 0.14 (MIT).
               libwebp vendored, built by cc, bindings pregenerated.
               Encoder only, lossy and lossless, ICC through the mux.
               Decoding stays with image-webp.
native-jxl     gamut-jxl 0.4 (MIT OR Apache-2.0) over gamut-jxl-sys, whose
               FFI is hand-written (no bindgen), over jpegxl-src 0.12
               (BSD-3-Clause), which vendors libjxl 0.12 with highway,
               brotli and skcms and builds it with cmake. The encode
               feature only; decoding stays with jxl-oxide.
native-avif    libavif 0.14 (BSD-2-Clause) over libavif-sys 0.17 and
               libaom-sys 0.17 (BSD-2-Clause), libavif 1.0.4 and libaom
               3.11 vendored and built with cmake, nasm on x86. codec-aom
               is the only codec: dav1d would need meson, rav1e is the
               portable encoder already. Encoder only; decoding stays
               with re_rav1d.
native-heif    libheif-rs 3.0 (MIT) over libheif-sys 5.3 (MIT), linking
               the system libheif >= 1.17 dynamically through pkg-config
               (vcpkg on Windows). Decoder only.
native-jpegli  jpegli 0.1 (BSD-3-Clause) over jpegli-sys 0.1
               (BSD-3-Clause), which vendors libjxl 0.10's jpegli and
               highway and builds them with cmake. Encoder only; decoding
               stays with zune-jpeg.
```

`native` turns all five on. `Format::encoder_features` lists `native-jpegli` next to `jpeg`, so a build without either names both in `EncoderUnavailable`.

Quality mappings, so the same `-q` means the same thing across a format's backends where the scales allow it:

```
webpx      quality 0..=100 one to one; effort 0..=10 to method 0..=6, default
           effort 6 = method 4 (cwebp's default, and what the seed table was
           calibrated at); lossy is always 4:2:0, other subsampling refused
gamut-jxl  quality to Butteraugli distance as cjxl does it (90 = 1.0,
           100 = 0.01, steeper below 30); effort 0..=10 to libjxl 1..=10 as
           effort + 1, so default 6 = libjxl's default 7
libavif    quality 0..=100 one to one; effort 0..=10 to speed 10..=0, the
           same inversion as ravif; Auto subsampling is 4:2:0 below 90 and
           4:4:4 from 90, the JPEG rule
jpegli     quality 1..=100 one to one; no effort knob; subsampling as JPEG
```

What each backend refuses instead of approximating: `libavif` and lossless (quality 100 through a YUV matrix is not lossless and the binding cannot select the identity matrix), `libavif` and an ICC profile (the binding cannot embed one; same rule as `ravif`), `webpx` and 4:4:4 lossy, `gamut-jxl` and chroma subsampling, `jpegli` and lossless. All return `Error::Unsupported`, never a silently different file.

## 3. Options considered

**JPEG XL binding.** `jpegxl-rs`: GPL, rejected. `libjxl-sys` + `kagamijxl` (ISC): libjxl 0.7 era, `bindgen` at build time, rejected as stale. `jxl-sys` 0.1 (MIT OR Apache-2.0): current libjxl but `bindgen` at build time and a bare `-sys` API, so it would need an `unsafe` wrapper of our own. `gamut-jxl`: current libjxl, safe API, hand-written FFI, permissive, but it pulls in `gamut-core` and `gamut-codec-abi` (both MIT OR Apache-2.0, both small) and asks for Rust 1.92. Chosen; the MSRV moved from 1.89 to 1.92 for the whole workspace rather than for one crate.

**WebP binding.** `webp` 0.3: old libwebp, no ICC, rejected. `webp-rs`: downloads prebuilt binaries in its build script, rejected. `webpx` 0.4 (imazen): current libwebp, ICC, `Limits`, an audited history, MSRV 1.89. Chosen. Its README recommends imazen's own `zenwebp` instead, which is AGPL and on the deny list; the recommendation is noted, not followed.

**AVIF.** `libavif` with `codec-aom` only. The default `codec-dav1d` needs meson and ninja on every builder for a decoder the portable tier already has, and `codec-rav1e` duplicates `ravif`'s encoder. The crate pins libavif 1.0.4; current libavif is 1.3. Acceptable for an encoder-only path; revisit when the crate moves.

**HEIC.** `libheif-rs` against a system library, not the crate's `embedded-libheif` build. libheif and libde265 are LGPL-3.0: dynamic linking keeps the LGPL obligations trivially met, and the embedded build ships no HEVC decoder anyway, which makes it a container parser that fails at decode. The cost is that `native-heif` is the one feature with a system dependency, and CI has to install it.

**jpegli.** `jpegli` 0.1 wraps libjxl 0.10.2's jpegli (early 2024). Newer jpegli lives inside the libjxl tree that `jpegxl-src` already vendors for `native-jxl`, but no crate exposes it through that build, so a native build compiles two libjxl trees. They link together without symbol clashes today; the alternative, a `jpegli-sys` pointed at `jpegxl-src`'s tree through `DEP_JXL_PATH`, is an upstream change. Not done here. The binding reports errors by unwinding out of C, so every call sits in `catch_unwind`; a `panic = "abort"` profile would turn a bad input into a process abort. The workspace does not set that.

**Pure-Rust alternatives.** `jixel` for JXL and `gamut-webp` for lossy WebP both exist under permissive licences. Neither has been measured against the C library it replaces. They belong to the portable tier's story, not this record's.

## 4. Trade-offs

**Build cost.** A `--features native` build compiles libwebp, libaom, libavif and two libjxl trees from source: about four minutes on a 12-core machine, and it needs cmake, a C++ compiler and nasm. `Swatinem/rust-cache` keeps the built archives between CI runs. The portable build is untouched.

**CI parity.** ADR-0001 asked for native jobs on all six desktop targets. What this record delivers:

```
x86_64 linux-gnu             native, all five, libheif and its libde265 plugin from apt
aarch64 linux-gnu            native-webp, native-jxl, native-avif, native-heif;
                             jpegli-sys's vendored libjxl 0.10 segfaults on the
                             first encode on the GitHub arm runner (GCC, Neoverse)
                             while the same code passes on Apple Silicon
x86_64 / aarch64 darwin      native, all five, libheif from brew; the Intel Mac
                             needs nasm 2, libaom 3.11's configure rejects nasm 3
x86_64 windows-msvc          native-webp, native-jxl, native-avif; native-heif needs
                             libheif from vcpkg and is not in CI, and native-jpegli
                             cannot share a cmake generator with native-jxl there
                             (jpegli-sys wants Ninja's single-config layout,
                             jpegxl-src wants the Visual Studio ClangCL toolset)
x86_64 linux-musl            native-webp, native-avif; the C++ backends need a
                             musl C++ toolchain that musl-tools does not provide
```

The gaps are documented in the README rather than papered over with a job that only proves "it compiles".

**Two libjxl copies** in a full native build, see above.

**Seed tables per backend.** Every native encoder has its own calibrated table, swept through `tools/calibrate --features native` on the same corpus as the portable ones. A native build therefore searches from a different starting point than a portable build for the same format, which is the intent: the same abstract quality means different things to `libavif` and `ravif`.

## 5. Consequences

What becomes easier: a native build writes every format ADR-0001 promised, lossy WebP and JPEG XL included, and reads HEIC. Adding a native backend is one module and one feature, as with the portable tier.

What becomes harder: the release matrix of ADR-0001 item 9 now has to build C code on every desktop target, and the Windows HEIC and musl C++ gaps have to be closed or shipped as documented omissions.

What to revisit:

- `jixel` as a portable JXL encoder, once measured. It would move JXL from "native only" to "portable, native for the last few percent".
- Building jpegli from the same libjxl tree as `native-jxl`.
- `libavif-sys` moving past libavif 1.0.

## 6. Action items

1. [x] `native-webp` over `webpx`, `native-jxl` over `gamut-jxl`, `native-avif` over `libavif` + `libaom`, `native-heif` over `libheif-rs`, `native-jpegli` over `jpegli`; round-trip, caps and golden tests for each; HEIC fixtures of the test pattern.
2. [x] CI jobs per desktop target with the feature sets above; `cargo deny` bans `jpegli-rs`, `jpegxl-rs` and `jpegxl-sys`.
3. [x] Run `tools/calibrate sweep --features native` and commit seed tables for `gamut-jxl`, `libavif` and `jpegli`. (Swept on 2026-09-11; the `webpx` table replaced the one measured through the `webp` crate.)
4. [ ] `native-heif` on Windows through vcpkg, `native-jpegli` on Windows once `jpegli-sys` finds its library under a multi-config generator, `native-jpegli` on aarch64 Linux once its libjxl tree stops crashing there, and the C++ backends on musl through a musl cross toolchain, in CI.
5. [ ] Measure `jixel` against `gamut-jxl` on the calibration corpus; if it is within a few percent at equal SSIMULACRA2, propose it for the portable tier in a new record.

---

## Sources

- [jpegxl-rs on crates.io](https://crates.io/crates/jpegxl-rs), licence field `GPL-3.0-or-later`
- [gamut-jxl on crates.io](https://crates.io/crates/gamut-jxl) and [the gamut repository](https://github.com/justin13888/gamut)
- [jpegxl-src on crates.io](https://crates.io/crates/jpegxl-src)
- [webpx on GitHub](https://github.com/imazen/webpx)
- [libavif-rs on GitHub](https://github.com/njaard/libavif-rs)
- [libheif-rs on GitHub](https://github.com/Cykooz/libheif-rs) and [libheif-sys](https://github.com/Cykooz/libheif-sys)
- [jpegli-rs (Szpadel) on GitHub](https://github.com/Szpadel/jpegli-rs)
- [jixel on GitHub](https://github.com/awxkee/jixel)
- [AOMedia patent licence](https://aomedia.org/license/patent-license/)
