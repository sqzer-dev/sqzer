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
crates/sqzer-wasm      browser build, portable tier only
docs/adr               decisions. Add a new numbered file, never edit an accepted one.
tests/fixtures         small test images, whole folder under 2 MB
```

Dependency direction is strictly downward: `core` depends on nothing in the workspace, `codecs` and `metrics` depend on `core`, `sqzer` depends on all three, `cli` and `wasm` depend on `sqzer` only. Do not add a reverse edge.

## Codec tiers and the licence line

Three Cargo feature groups in `sqzer-codecs`:

- `portable`: pure Rust, permissive licence, must build on `wasm32-unknown-unknown`. Default.
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
- Parallelism is per file, in the CLI, with `rayon`. Encoders get an explicit thread budget. Nothing in `sqzer-core` spawns threads. `rayon` is compiled out on wasm.
- `#![forbid(unsafe_code)]` is set at the workspace level. Do not add `unsafe`; if a backend needs it, the backend crate has it, not us.

## Commands

```sh
cargo build --workspace                                   # portable tier
cargo build -p sqzer-cli --features native                # needs C libs on PATH / vcpkg
cargo build -p sqzer-wasm --target wasm32-unknown-unknown # proves portable stays C-free
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
- A behaviour change updates `CHANGELOG.md` under Unreleased in the same PR. The file follows the Angular changelog convention: `### Features` / `### Bug Fixes` / `### Performance Improvements` headings, entries as `* **scope:** subject` with the same crate scope as the commit.
- Architecture changes get a new ADR in `docs/adr`, not a rewrite of an existing one.
- Releases: push a `v*` tag. `dist-workspace.toml` drives `.github/workflows/release.yml`; edit the TOML and run `dist generate`, never the workflow by hand. The release `plan` job fails a PR when the two disagree.
- New dependencies: state the licence in the PR description and check it is on the `deny.toml` allow-list. Prefer crates that already speak `imgref` or implement `image`'s traits, since adapters then come cheap.

## Testing expectations

- Every backend has a round-trip test on the fixtures and a test that its `EncoderCaps` are truthful (claims alpha, encodes alpha).
- Every encoder has a golden test: encode a fixture at a fixed quality, check SSIMULACRA2 against a committed score with a tolerance. A dependency bump that degrades output must fail CI.
- The target search has explicit tests for the failure modes: tiny image, flat image, pure noise, target unreachable at the ceiling.
- No coverage gate. Percentages say nothing about whether the output looks right.

## Writing style for anything user-facing

Applies to README, docs, rustdoc, CLI help and error messages, changelog, PR descriptions.

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