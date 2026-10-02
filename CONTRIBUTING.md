# Contributing to sqzer

Bug reports, fixes, new backends and better defaults are all welcome. `sqzer` is maintained by one person, so a small, focused change with a test gets reviewed much faster than a large one.

## Language

English is preferred for issues, pull requests and discussions; if you can, add an English version next to the original. Machine translation is fine, and so is imperfect English. Commit messages and code comments are in English.

## Before you start

- Anything bigger than a small fix: open an issue or a discussion first, so we agree on the approach before you spend time on it. [`ROADMAP.md`](ROADMAP.md) has what is planned and what is not.
- Architecture lives in [`docs/adr`](docs/adr). Read [`0001-system-design.md`](docs/adr/0001-system-design.md) before touching the crate layout, the codec traits or the pipeline. A change to a decision gets a new numbered ADR; an accepted one is never edited. A new ADR comes with its row in [`docs/adr/README.md`](docs/adr/README.md) and an entry in `.greptile/files.json`, so the review bot reads it too.
- Usage questions go to [Discussions](https://github.com/sqzer-dev/sqzer/discussions/categories/q-a), not issues.
- Security problems go through [private vulnerability reporting](https://github.com/sqzer-dev/sqzer/security/advisories/new), never a public issue. See [`SECURITY.md`](SECURITY.md).

## Development setup

The toolchain, the native tier's C dependencies and the calibration harness are covered under [Development in the README](README.md#development). The short version:

```sh
# portable tier: pure Rust apart from the vendored libdeflate, which needs
# a C compiler: gcc, clang or MSVC on desktop targets, clang for wasm32
cargo build --workspace
cargo test --workspace

# native tier: cmake, a C++ compiler and nasm on PATH
cargo build -p sqzer-cli --features native
```

## Checks

CI runs all of these, plus the six desktop targets and the `wasm32` build. Run them before asking for review:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features    # pedantic is on, warnings fail CI
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check

# proves the portable tier builds for a target with no libc; needs clang
cargo build -p sqzer-wasm --target wasm32-unknown-unknown

# the browser build's JavaScript API over the fixtures, in Node; needs wasm-pack
(cd crates/sqzer-wasm && wasm-pack test --node)
```

> **Note**: The golden tests compare each encoder's SSIMULACRA2 score on the fixtures against a committed value. If a change moves a score, say why in the PR; do not just update the number.

## Rules the code follows

- Dependencies point downward only: `sqzer-core` depends on nothing in the workspace, `sqzer-codecs` and `sqzer-metrics` on `sqzer-core`, `sqzer` on those three, `sqzer-cli` and `sqzer-wasm` on `sqzer`.
- `unsafe` is forbidden in the workspace. The only exceptions are the three HEIC binding crates (`heif-dl`, `heif-imageio`, `heif-wic`).
- No hand-written codecs, resamplers, colour transforms or metrics. Mature crates exist for each; `sqzer` is the traits and the pipeline around them.
- Adding a format must not need changes to the CLI, the search, the presets or the JSON output. A backend implements `Decoder` or `Encoder`, describes itself with `DecoderCaps` or `EncoderCaps`, and takes codec-specific knobs through `codec_specific`.
- A format with no encoder in the build returns `Error::EncoderUnavailable`. Never fall back to another format or a worse encoder.
- Every decoder respects `DecodeOpts::max_pixels`.

## Licences and new dependencies

`sqzer` is `MIT OR Apache-2.0`, and every dependency is under a licence on the [`deny.toml`](deny.toml) allow-list: permissive licences plus MPL-2.0. In a PR that adds a dependency:

- state its licence in the PR description and check it is on the allow-list
- prefer crates that already speak `imgref` or implement `image`'s traits
- put a C binding behind a `native-*` feature, never in the portable tier

AGPL and GPL crates are not accepted, and neither are the imazen `zen*` crates listed in `deny.toml`. Loosening `deny.toml` to make a check pass is not a fix.

Unless you say otherwise, any contribution you submit is dual-licensed as `MIT OR Apache-2.0`, as defined in the Apache-2.0 licence, without additional terms.

## Tests

- A new backend gets a round-trip test on the fixtures, a test that its caps are truthful (claims alpha, encodes alpha) and, for an encoder, a golden SSIMULACRA2 test.
- A bug fix gets a test that fails without it.
- Fixtures stay small: the whole `tests/fixtures` folder is under 2 MB. Larger corpora are downloaded, never committed.

## Pull requests

- Branch from `main`, open a PR, and it is squash-merged. Intermediate commits on the branch do not matter.
- The PR title becomes the commit message, so it uses a conventional prefix scoped to the crate: `feat(codecs): mozjpeg-rs encoder`, `fix(cli): exit code on partial batch failure`, `docs: ...`.
- A change in behaviour adds a line to `CHANGELOG.md` under `## Unreleased`, as `* **scope:** subject` under `### Features`, `### Bug Fixes` or `### Performance Improvements`.
- Docs, rustdoc, help text and error messages follow the house style: sentence case headings, backticks on every identifier, flag and path, examples as commands with a `#` comment instead of a paragraph, no tables, no emoji.

## Code of conduct

Everyone taking part is expected to follow the [code of conduct](CODE_OF_CONDUCT.md). Reports go to `conduct@sqzer.dev`.
