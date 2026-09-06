# ADR-0002: `libdeflate` in the portable tier

**Status:** Proposed
**Date:** 2026-09-06
**Deciders:** Vlad (sole maintainer)
**Scope:** One exception to the "portable means pure Rust" rule of ADR-0001 D2, and how it is fenced.

---

## Context

ADR-0001 names `oxipng` as the PNG encoder for the portable tier and describes PNG as "fully solved, no C needed". The second half is wrong. `oxipng` has an unconditional dependency on `libdeflater`, a binding to `libdeflate`, which is a C library. The sources are vendored and built by the `cc` crate, so nothing has to be installed on desktop targets. On `wasm32-unknown-unknown` the C code needs a compiler that targets wasm. Squoosh has one: its `codecs/rust.Dockerfile` copies `clang`, its runtime libraries and the musl libc headers out of `emscripten/emsdk` into the Rust image, sets `CPATH` to those headers, and builds `oxipng` with `default-features = false, features = ["freestanding"]`, which compiles `libdeflate` with `-ffreestanding -nostdlib`. So `oxipng` does build for wasm32, but not with a plain Rust toolchain: the wasm32 CI job here installs only `rustup` and the target, on purpose, and the portable tier is defined as pure Rust.

There is no pure-Rust replacement for what `oxipng` does. The `png` crate writes a correct file with adaptive filtering and a `flate2` stream, and that is all. The filter-strategy search, colour-type and bit-depth reduction, palette and alpha optimisation, and the `zopfli` pass are what make `oxipng` output materially smaller than a stock `png` file. Writing that search here is ruled out by the project rules (never write an encoder), and giving `oxipng` a pure-Rust DEFLATE backend is upstream work this project does not control.

## Decision

`oxipng` is the PNG encoder in the `png` feature of `sqzer-codecs`, with `libdeflate` accepted as the one C dependency in the portable tier, under these conditions:

- The dependency is target-gated: `oxipng` is listed under `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`. The wasm32 build compiles the plain `png` writer instead and registers it for the same format. The wasm CI job therefore still proves that no C dependency reaches the browser build.
- No system library is ever required. `oxipng` is built with `default-features = false`, which rules out the `system-libdeflate` feature, so `cargo build` needs only a working C compiler, which every desktop CI runner and every Rust installation already has.
- `parallel` stays off. `oxipng`'s rayon pool cannot be given a thread budget, and ADR-0001 D3 makes encoders single-threaded until the pipeline hands out one.
- `zopfli` (Apache-2.0, pure Rust) is on and used only at effort 10.

The plain `PngEncoder` stays public in `sqzer_codecs::png` for callers who want the fast path on any target.

## Options considered

1. `oxipng` under the `native` tier as `native-png`. Keeps the portable tier honest but makes the default build's PNG output worse than the tool it replaces, and PNG is the one format where the portable tier is supposed to be best in class. Rejected.
2. `oxipng` everywhere, with `libdeflate`'s freestanding mode on wasm32, the way Squoosh does it. Needs `clang` with a wasm target plus a set of libc headers in the wasm CI job and on every machine that builds the browser package, and it turns that job from "proves no C gets in" into "proves the C we chose compiles". Not verifiable on a machine without `clang`, which is where this record was written. Rejected for now; it is the natural upgrade if `wasm32` PNG output ever matters, and Squoosh's Dockerfile is the recipe.
3. Chosen: `oxipng` on desktop targets, `png` on wasm32.

## Trade-offs

- The desktop portable build now needs a C compiler. It needed one before for nothing; every supported target's CI runner has one, and `cargo` users on Windows with the MSVC toolchain have `cl.exe`.
- The wasm32 build writes larger PNGs than the desktop build from the same input. The browser package is a demo surface, not the batch tool, so the gap is acceptable and documented in the README.
- Building for a target whose C ABI differs from the host now needs a matching C compiler. The musl job is the one case in CI: it runs on a glibc runner, `cc` looks for `x86_64-linux-musl-gcc` there and finds nothing, so the job installs `musl-tools` and points `CC_x86_64_unknown_linux_musl` at `musl-gcc`. The same applies to anyone building `x86_64-unknown-linux-musl` locally, and to the `cargo-dist` matrix when it lands (ADR-0001 item 9). The other five desktop targets build on native runners with their own toolchain and need nothing.

## Consequences

- ADR-0001's "no C needed" sentence about PNG is superseded by this record. ADR-0001 is not edited.
- The README states the exception next to the tier table.
- If `oxipng` ever gains a pure-Rust DEFLATE backend, drop the target gate, register `OxipngEncoder` on wasm32 too, and mark this record superseded.

## Action items

1. [x] Gate `oxipng` to non-wasm32 targets and register `PngEncoder` on wasm32.
2. [x] Document the exception in the README.
3. [ ] Revisit when `oxipng` offers a pure-Rust DEFLATE backend or when a browser build needs optimised PNG output.
