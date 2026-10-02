# CLAUDE.md

Guidance for Claude Code working in this repository. Read `docs/adr/0001-system-design.md` before touching architecture; it is the source of truth and this file only summarises it.

## What this is

`sqzer` is a multi-format image optimizer: a Rust library, a CLI and later a WASM package. The goal is Squoosh-quality defaults as a scriptable tool. Pure Rust by default, C codecs opt-in, AGPL codecs never in the library or default builds.

Solo-maintained. Prefer fewer, well-chosen dependencies over coverage. Never write a decoder, encoder, resampler, colour transform or metric; mature crates exist for each and the job here is the container, the traits and the pipeline around them.

## Layout

```
crates/sqzer-core      image model, codec traits, params, errors. No codecs, no I/O.
crates/sqzer-codecs    every backend behind a feature flag, tiered (see below)
crates/sqzer-metrics   SSIMULACRA2 scoring and the target-quality search
crates/sqzer           library facade. This is the public API.
crates/sqzer-cli       the binary, named `sqzer`
crates/sqzer-wasm      browser build: the JavaScript API of the npm package, portable tier minus `svg`
crates/sqzer-native-tier  what `native` means per target, features only, no code
crates/heif-dl         HEIC through a runtime-loaded `libheif`. Holds `unsafe` (ADR-0005)
crates/heif-imageio    HEIC through ImageIO on macOS. Holds `unsafe`
crates/heif-wic        HEIC through WIC on Windows. Holds `unsafe`
crates/sqzer-codecs-agpl  reserved, empty, AGPL. Never a dependency of anything here
docs/adr               decisions. Add a new numbered file, never edit an accepted one.
tests/fixtures         small test images, whole folder under 2 MB
```

Dependency direction is strictly downward: `core` and the three `heif-*` crates depend on nothing in the workspace, `codecs` depends on `core` and the `heif-*` crates, `metrics` on `core`, `native-tier` on `codecs` (to forward features), `sqzer` on `core`, `codecs`, `metrics` and `native-tier`, `cli` and `wasm` on `sqzer` only. Do not add a reverse edge.

## Codec tiers and the licence line

Three Cargo feature groups in `sqzer-codecs`:

- `portable`: permissive licence, no system library, must build on `wasm32-unknown-unknown`, which has no libc. Pure Rust apart from vendored C built by `cc` (`libdeflate` for `oxipng`), so the wasm32 build needs `clang` (ADR-0011 D1). Default.
- `native`: C bindings (`native-webp`, `native-jxl`, `native-avif`, `native-heif`, `native-jpegli`). Opt-in, desktop only. The crate choices are in `docs/adr/0004-native-tier.md`; `jpegxl-rs` is GPL and banned.
- `agpl`: reserved, empty. When populated it lives in a separate `sqzer-codecs-agpl` crate.

`native` on `sqzer` and `sqzer-cli` means the backends the target can build and run, decided per target in `crates/sqzer-native-tier/Cargo.toml` (ADR-0006); a single `native-*` feature is strict. Release binaries are built with that one feature list on all six desktop targets. The per-target facts live in that manifest and in `crates/sqzer-cli/src/native_set.rs`; change both together.

`deny.toml` enforces this. The allow-list is permissive licences plus MPL-2.0. The ban list names the imazen zen* crates (`zenwebp`, `zenjpeg`, `zenavif`, `zenravif`, `zenjxl`, `jxl-encoder`, `rav1d-safe`, `heic`). Do not add any of them to a permissive crate, do not add AGPL to the allow-list, and do not work around `cargo deny` failures by loosening the config. If a task needs one of those crates, stop and say so.

When a requested output format has no encoder in the current build, return `Error::EncoderUnavailable { format, available_in }`. Never fall back silently to a different format or a worse encoder.

## Core design rules

- One intermediate type, `sqzer_core::image::Image`: interleaved samples, `u8`/`u16`/`f32`, ICC kept on the struct, orientation already applied. Backends adapt to and from it; the pipeline never sees a backend's own buffer type.
- Backends implement `Decoder` and `Encoder` from `sqzer_core::codec` and describe themselves with `EncoderCaps`. Adding a format must not require changes to the CLI, the metric loop, presets or JSON output.
- `EncodeParams` is codec-agnostic. Each backend maps abstract quality (0 to 100) to its own scale internally. Backend-only knobs go through `codec_specific`, never as new struct fields.
- Default mode is a perceptual target (`Target::Ssimulacra2(70.0)`), searched by bisection with a cap of 6 encodes. Explicit `Target::Quality` disables the search.
- Metadata is stripped by default. ICC is converted to sRGB unless `keep_icc`. EXIF orientation is applied then removed.
- Decompression-bomb guard: `DecodeOpts::max_pixels`, default 268 megapixels. Respect it in every decoder.
- Parallelism is per file, in the CLI, with `rayon`. Encoders get an explicit thread budget. Nothing in `sqzer-core` spawns threads. On wasm nothing spawns at all: `rayon` is in the graph through `rav1e` and runs sequentially.
- `#![forbid(unsafe_code)]` is set at the workspace level. Do not add `unsafe` anywhere else; if a backend needs it, it gets a binding crate of its own with a safe API, the way the three `heif-*` crates do.

## Commands

```sh
cargo build --workspace                                   # portable tier
cargo build -p sqzer-cli --features native                # needs C libs on PATH / vcpkg
cargo build -p sqzer-wasm --target wasm32-unknown-unknown # proves portable builds without a libc; needs clang
(cd crates/sqzer-wasm && wasm-pack test --node)          # the JavaScript API over the fixtures; needs wasm-pack, and no RUSTFLAGS set
crates/sqzer-wasm/pack.sh                                 # the npm package as a tag publishes it, packed and run from its tarball
cargo test --workspace
cargo clippy --workspace --all-targets --all-features     # pedantic is on, warnings are errors in CI
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check
```

Run the full set before declaring a change done. CI runs the same commands plus six desktop targets and the wasm build.

## Workflow

- `main` is protected and always green. Work on a branch, open a PR, squash-merge.
- Squash commit messages use conventional prefixes scoped to the crate: `feat(codecs): mozjpeg-rs encoder`, `fix(cli): exit code on partial batch failure`. Intermediate commits on a branch do not matter.
- A behaviour change updates `CHANGELOG.md` under Unreleased in the same PR. The file follows the conventional-changelog convention: `### ⚠️ BREAKING CHANGES` first when a release breaks the library API or the CLI, then `### Features` / `### Bug Fixes` / `### Performance Improvements`, entries as `* **scope:** subject` with the same crate scope as the commit. A breaking entry says what to do instead. The `⚠️` heading (U+26A0 U+FE0F, the colour emoji form; pasting often drops the U+FE0F) is the one exception to the emoji and sentence-case rules below.
- Architecture changes get a new ADR in `docs/adr`, not a rewrite of an existing one. A new ADR also gets a row in `docs/adr/README.md`, an entry in `.greptile/files.json` scoped to the code it governs, and an update to any `.greptile/config.json` rule it changes, in the same PR.
- Releases: push a `v*` tag on the tip of `main`, and merge nothing that touches `.github/workflows` until the release run finishes: GitHub refuses to create the release from `GITHUB_TOKEN` when `main` has workflow changes the tagged commit lacks (ADR-0006 item 5). `dist-workspace.toml` drives `.github/workflows/release.yml`; edit the TOML and run `dist generate`, never the workflow by hand. The release `plan` job fails a PR when the two disagree. The same tag publishes `crates/sqzer-wasm` to npm as `sqzer`: `release.yml` calls `.github/workflows/publish-npm.yml`, which runs `crates/sqzer-wasm/pack.sh` and publishes through npm's trusted publishing, no token stored (ADR-0011 D5). The trusted publisher on npmjs.com names `release.yml`, the calling workflow.
- New dependencies: state the licence in the PR description and check it is on the `deny.toml` allow-list. Prefer crates that already speak `imgref` or implement `image`'s traits, since adapters then come cheap.

## Testing expectations

- Every backend has a round-trip test on the fixtures and a test that its `EncoderCaps` are truthful (claims alpha, encodes alpha).
- Every encoder has a golden test: encode a fixture at a fixed quality, check SSIMULACRA2 against a committed score with a tolerance. A dependency bump that degrades output must fail CI.
- `crates/sqzer-wasm/src/tests.rs` runs on `wasm32` only, under `wasm-pack test --node`: the package's API as JavaScript calls it, and the portable golden scores again on that target. Its golden table repeats the portable rows of `crates/sqzer/tests/golden.rs`; change both together.
- The target search has explicit tests for the failure modes: tiny image, flat image, pure noise, target unreachable at the ceiling.
- No coverage gate. Percentages say nothing about whether the output looks right.

## Writing style for anything user-facing

Applies to README, docs, rustdoc, CLI help and error messages, changelog, PR descriptions, `CONTRIBUTING.md`, `SECURITY.md`, the issue forms and the PR template. `LICENSE-*` and `CODE_OF_CONDUCT.md` are verbatim third-party texts: never edit them for style.

- Sentence case headings. Short prose, then a fenced block that carries the detail. Explanation goes in `#` comments above the command, not in a paragraph describing it.
- Backticks on every identifier, crate name, flag and path.
- No em dashes. Use a spaced hyphen, a colon or two sentences.
- No emoji. No tables; use fenced blocks or plain text.
- Raw output over reformatted output: paste the `hyperfine` block, then one sentence of interpretation.
- State uncertainty flatly and name the next step. Do not hedge, do not overclaim.
- `> **Note**:` blockquotes for gotchas.
- Benchmarks and numbers may be enthusiastic. Everything else is not.

## Do not

- Do not add `image`'s stock encoders as a backend. They are what this project replaces.
- Do not make `sqzer-core` depend on any codec, `image`, `zune-image` or `zencodec`. Adapters live in `sqzer-codecs`.
- Do not add a fixed per-format quality as the default path. The perceptual target is the product.
- Do not commit anything to `tests/fixtures` that pushes the folder over 2 MB. The calibration corpus is downloaded, never committed.
- Do not put pre-commit hooks in the repo.
- Do not edit `Cargo.lock` by hand or run a blanket `cargo update` inside a feature PR.