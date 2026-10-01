# ADR-0011: The browser build

**Status:** Accepted
**Date:** 2026-10-01
**Deciders:** Vlad (sole maintainer)
**Scope:** ADR-0001 action item 10 and the 0.3 entry of `ROADMAP.md`: `sqzer-wasm` as an npm package and a page on `sqzer.dev` you can drop an image on. Decides what "portable" requires of a build machine, which supersedes the wasm32 gate of ADR-0002; how AVIF decoding reaches wasm32; the JavaScript API; the build flags; how the package is published from a tag; and where the page lives. Nothing here changes the tier rules of ADR-0001 D2 for licences, the pipeline, the defaults policy or the CLI.

---

## 1. Context

`crates/sqzer-wasm` re-exports `Sqzer` and nothing else. Its job so far was to make the wasm32 CI job fail if a C dependency reached the portable tier. To find out what the real package would cost, a scratch crate exporting `optimize(bytes, format?, quality?)` through `wasm-bindgen` 0.2.128 (already in `Cargo.lock` through `v_frame`) was built for `wasm32-unknown-unknown` and run in Node 24 over every fixture and over synthetic photos of 2 and 12 megapixels. All numbers below are from one WSL2 machine on 2026-09-30 and 2026-10-01.

### What works unchanged

Every fixture decoded and encoded, the SSIMULACRA2 search ran, and `rav1e`'s use of `rayon` fell back to the current thread as `rayon-core` documents for wasm32. Against the portable-only native CLI on the same 2 megapixel JPEG:

```text
case               portable native   wasm32 +simd128   wasm32 no simd
jpeg, target 70     745 ms            957 ms            1652 ms
jpeg -q 80          148 ms            185 ms             208 ms
webp lossless        48 ms             72 ms              85 ms
avif -q 50         3688 ms           4793 ms            5489 ms
12 MP jpeg, target 70                                  17.9 s, 1.7 GB peak
```

The native column includes process start and a file write, so the true gap is a little over 1.3x. `simd128` matters because `fast-ssim2` selects its `wasm128` path by target feature, and it costs nothing in reach: `yuv` and `fast_image_resize` compile `v128` functions unconditionally, so even a build without the flag fails to validate in an engine without SIMD. Every browser has had wasm SIMD since Safari 16.4.

### Size

One binary with the whole portable tier, fat LTO, stripped, and what dropping one feature at a time saves:

```text
build                        raw       brotli
full portable tier           8.48 MB   1.94 MB
without svg (resvg)          6.37 MB   1.37 MB
without avif (rav1e)         6.77 MB   1.64 MB
without jxl-decode           7.41 MB   1.64 MB
without exr                  8.39 MB   1.86 MB
jpeg + png + webp only       2.07 MB   0.61 MB
jpeg + png + webp + avif     4.11 MB   0.91 MB
```

`wasm-opt -Oz` takes 8 % off the raw size and nothing off the compressed size. For scale, jSquash ships `avif_enc` at 3.4 MB and `mozjpeg_enc` at 0.25 MB per package.

### What was broken or missing

- AVIF input is "unrecognised image format" on wasm32, not "no decoder": `register_portable` skips the decoder, and the probe with it, so the README's claim that the wasm build recognises AVIF is wrong. `re_rav1d`, the decoder on desktop, was archived on 2026-06-09. Upstream `rav1d` merged a safe `dav1d-rs`-style Rust API on 2026-04-04 (`rav1d::{Settings, Decoder, Picture}`, the same calls `avif/decode.rs` makes today) but has not released since 1.1.0 of May 2025. Its `main` fails on wasm32 with 35 errors, every one a `libc` name: `ptrdiff_t`, `intptr_t`, `uintptr_t`, `off_t`, seven errno constants, `malloc`, and an inference cascade from the missing `ptrdiff_t`. With a 15-line shim module providing those names from `core`, `main` builds for wasm32, and decodes every AVIF fixture and two 2 megapixel files with plane sums identical to the native build of the same code, in 1.4 MB of wasm, at about 1.5x native time.
- `oxipng` is compiled out on wasm32 (ADR-0002) because `libdeflate` is C. ADR-0002 assumed Squoosh's recipe was needed, `clang` plus musl headers on `CPATH`, and recorded that it could not be checked on a machine without `clang`. Checked now: `oxipng 10.2.1` with `default-features = false, features = ["freestanding", "zopfli"]` builds for wasm32 with `clang` on `PATH` and nothing else, no `CC`, no headers. `cc` finds `clang` on its own. The result optimises an 800x600 PNG byte-identical to the native build at levels 2 and 4, 1.7x slower, in 452 KB of wasm. GitHub's `ubuntu-24.04` runner image ships clang 16 to 18. On the 2 megapixel photo the plain `png` writer's file was 14 % larger than `oxipng`'s.
- The facade does not re-export `Format` or `Target`; a binding reaches them through `sqzer::core`.
- The 12 megapixel search peaked at 1.7 GB, which is ADR-0008's 135 bytes per pixel. A wasm32 module tops out at 4 GiB, so the default `max_pixels` of 268 megapixels means nothing there.

### The ecosystem

- `wasm-bindgen` 0.2.129 (2026-09-25, MSRV 1.81). `wasm-pack` moved to the `wasm-bindgen` organisation and is active (0.15.0, 2026-05-15); it downloads the `wasm-bindgen` CLI matching `Cargo.lock`, writes `package.json` from `Cargo.toml`, and since 0.14 runs `wasm-opt` only when asked.
- Threads: `wasm-bindgen-rayon` still needs nightly and `build-std`, and `SharedArrayBuffer` needs `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy` headers, which GitHub Pages cannot send.
- `cargo-dist` cannot build wasm, but `publish-jobs = ["./name"]` runs a reusable workflow from `.github/workflows` after the release is hosted, with the plan as input. npm trusted publishing takes an OIDC token from the workflow (`id-token: write`, `npm publish --provenance`) instead of a stored secret and needs npm 11.5.1, which Node 24 carries.
- The npm name `sqzer` is held by a 0.0.1 placeholder published 2026-09-06; its `license` field says ISC. `sqzer.dev` resolves to a GoDaddy parking redirect. GitHub Pages is not enabled on the repository.
- Squoosh is alive (pushed 2026-09-23, 262 open issues), still without releases or a batch mode. jSquash publishes one ESM package per codec, Apache-2.0, last released May 2025.

---

## 2. Decision

### D1. Portable means it runs everywhere, not that it builds with `rustc` alone

ADR-0001 D2 said "pure Rust" and ADR-0002 made one exception on desktop and none on wasm32. The rule as it has actually been applied is:

```text
portable   permissive licence; no system library at build or run time; output identical on
           every target. Vendored C built by `cc` is allowed, so a C compiler may be needed
           to build: gcc, clang or MSVC on desktop, as today, and clang for wasm32.
native     C libraries that cannot go everywhere. Opt-in, desktop only, as before.
```

Under that rule `oxipng` is the PNG encoder on every target, with `freestanding` enabled through a `[target.'cfg(target_arch = "wasm32")'.dependencies]` entry. The `png`-writer fallback and the `cfg(not(target_arch = "wasm32"))` gates in `sqzer-codecs` go. This supersedes the wasm32 gate of ADR-0002; that record's reasoning about `libdeflate` on desktop stands.

The wasm CI job changes meaning. It proved that no C reaches the browser build; now it proves that the browser build compiles for `wasm32-unknown-unknown`, a target with no libc. That is the whole check: C that needs a libc, or any header one provides, fails to compile there on its own, which is exactly how `oxipng` failed before `freestanding`. No allow-list of which crates may build C, and nothing to keep in step with the dependency graph.

Anyone building `sqzer` for wasm32 needs `clang`. That is documented in the README, `CONTRIBUTING.md` and `CLAUDE.md` next to the desktop C compiler requirement it mirrors.

### D2. AVIF decoding through a published fork of upstream `rav1d`

`sqzer-codecs` moves from `re_rav1d` to `sqzer-rav1d` on every target: upstream `rav1d` `main` plus the `libc` shim, published from `github.com/sqzer-dev/rav1d`, BSD-2-Clause, assembly off, `bitdepth_8` and `bitdepth_16`. The fork's diff is the shim and the package metadata, nothing else, so a rebase on upstream is minutes. `avif/decode.rs` changes the crate path and `send_data`'s `Vec<u8>` to `Box<[u8]>`. AVIF then decodes on wasm32, the default output format there is AVIF like everywhere else, and a perceptual target for AVIF works.

The exit is upstream: the shim goes to `memorysafety/rav1d` as a pull request, and when a release carries both the Rust API and the wasm32 fix, `sqzer-codecs` depends on `rav1d` directly and the fork is archived. Until then the `paste` advisory ignore in `deny.toml` is dropped if upstream `main` no longer pulls it, and kept otherwise.

> **Note**: crates.io refuses git dependencies, so a fork means a published crate. This is the `re_rav1d` pattern with the sqzer organisation holding the crate instead of a third party.

### D3. One package, `sqzer` on npm, the portable tier minus SVG, the browser's canvas for input it cannot decode

The package is `sqzer` on npm, an ES module built with `wasm-pack build --target web`, which every bundler and the bare browser load, and its version is the workspace version. It carries the portable tier minus `svg`: every raster decoder, `mozjpeg-rs`, `oxipng`, `image-webp` lossless and `ravif`. That is 1.4 MB over brotli by the table above; a page loads it once, and one package with one capability list is easier to explain than a feature matrix. Feature subsets are the answer if size ever becomes the complaint, not part of this record.

SVG is left to the browser, under a rule that covers more than SVG: the package's own decoder comes first, the browser's canvas decodes what the package cannot, and the canvas is never used inside the search loop. `resvg` on wasm32 has no fonts, so for SVG the browser is the more capable decoder today, and its output is the same 8-bit sRGB `resvg` would give. For HEIC on Safari the browser is the only decoder, at 8 bits. When a build carries a more capable decoder, the AGPL build of `ROADMAP.md` with its HEIC decoder for one, the fallback stops firing for that format on its own, because it only runs where the package has nothing. `sqzer-wasm` selects the codec features itself, as `sqzer-codecs` features under `sqzer` with default features off, so the wasm build and the library's `portable` feature can differ by that one entry.

The API is the builder, as ADR-0001 D8 said, spelled the way JavaScript spells it. Three calls:

```ts
import init, { optimize, decode, decodeAny, fromPixels, codecs } from "sqzer";
await init();

// everything in one call: probe, decode, prepare, resize, encode
const out = optimize(bytes, { format: "avif", target: 70, width: 1600 });

// decode once, encode as often as a slider moves
const image = decode(bytes, { maxPixels: 24_000_000 });
// the same, with the browser's canvas for what `decode` cannot read: SVG, HEIC on Safari
const image2 = await decodeAny(bytes, { maxPixels: 24_000_000, width: 1600 });
// pixels from anywhere else, a canvas say
const image3 = fromPixels(rgba, width, height);
image.width; image.height; image.format; image.alpha; image.animated;
const a = image.encode({ format: "webp", lossless: true });
const b = image.encode({ format: "jpeg", quality: 80, onTrial: (t) => {} });
image.free();

// what this build decodes and encodes, and from which tier, as `--list-codecs`
codecs();
```

`options` is a plain object whose keys are the CLI's flags in camel case: `format`, `target`, `quality`, `lossless`, `preset`, `effort`, `subsampling`, `keepIcc`, `keepMetadata`, `fast`, `width`, `height`, `fit`, `position`, `background`, `scale`, `enlarge`, `filter`, `maxPixels`, and `codecOpts` as `{ "avif:bit_depth": "8" }` for `-x`. `onTrial` receives `Progress::Trial` as `{ n, max, quality, score }`. The result is the `--json` record of ADR-0003 with `bytes` added, so the same words mean the same things in both: `format`, `backend`, `tier`, `inputFormat`, `width`, `height`, `alpha`, `content`, `outputWidth`, `outputHeight`, `quality`, `lossless`, `score`, `reached`, `trials`. An error is a `SqzerError` whose `kind` is the `sqzer_core::Error` variant name and whose `availableIn` lists the features for `EncoderUnavailable` and `DecoderUnavailable`.

Options and results cross the boundary through `serde-wasm-bindgen`, and their TypeScript types are generated by `tsify-next`, so `sqzer.d.ts` is complete without a hand-written file. Both are MIT or Apache-2.0. `console_error_panic_hook` turns a panic into a readable exception.

The package is synchronous and single-threaded, like the library. A page runs it in a Web Worker; the package does not ship one, since how a worker is bundled belongs to the consumer.

### D4. Build flags and limits

- `simd128` is on for every wasm32 build of the workspace, through `.cargo/config.toml`, so the CI job, `wasm-pack` and a `cargo build --target wasm32-unknown-unknown` agree.
- No threads. `rayon` stays compiled in through `rav1e` and runs sequentially; nothing is spawned.
- `maxPixels` defaults to 24 megapixels in the package, against 268 in the library. The search reserves about 160 bytes per pixel (ADR-0008), and 24 megapixels of that is under the 4 GiB a wasm32 module can address with room for the decoded input and the output. `DecodeOpts::max_pixels` is unchanged; the package sets it.
- `wasm-opt` stays off. It saved nothing over brotli in the measurement; whether `-O` speeds the code up is an action item, not a decision.
- The crate stays `publish = false` on crates.io and becomes `cdylib` only. Its Rust API is the npm package, not a library.

### D5. Published from the tag by a `dist` publish job

`dist-workspace.toml` gains `publish-jobs = ["homebrew", "./publish-npm"]`. `.github/workflows/publish-npm.yml` is a reusable workflow that checks out the tagged commit, installs the wasm32 target and `wasm-pack`, builds the package, and runs `npm publish --provenance --access public` under `id-token: write` with Node 24. The trusted publisher on npmjs.com names this repository and that workflow file; no token is stored. `dist generate` rewrites `release.yml` to call it, and the `plan` job keeps checking that the two agree. The job also attaches the `.tgz` to the GitHub release so a package exists even if npm is down.

The first real publish replaces the placeholder's metadata: `wasm-pack` writes `license`, `repository` and `description` from `Cargo.toml`.

### D6. The page: its own repository, `sqzer-dev/sqzer.dev`, on GitHub Pages

A static page in a repository of its own, `sqzer-dev/sqzer.dev`: plain HTML, one ES module and one worker script, no framework and no bundler. It depends on the published package by version, `sqzer@0.3.0` from npm through jsDelivr, never on a checkout of this repository. It starts in its own repository because of where `ROADMAP.md` says it ends: the full browser build over `sqzer-codecs-agpl` is AGPL-3.0, and so is any page that bundles it. A page under that licence cannot live in a tree licensed MIT or Apache-2.0, and moving it later would mean carrying its history out and relicensing it in place. The page is MIT or Apache-2.0 until the AGPL package exists, and the version it imports is the only line that changes when it switches. It offers a drop zone (also paste and a file picker), the output format, the target with a quality alternative, a width, before-and-after with the sizes and the score, and download. Encoding runs in the worker with `onTrial` driving a progress line. It sends nothing anywhere: no analytics, no error reporting.

That repository's Pages workflow deploys on every merge to its `main`, so a page change never waits for a release of the crates. A release here is followed by a one-line pull request there bumping the version the page imports. That step is written into the release procedure of `CLAUDE.md`, the Releases bullet under Workflow, with action item 5, and the page repository runs Dependabot on its `sqzer` dependency as the backstop, so a bump nobody made still arrives as a pull request. This repository's release workflow does not touch the page. `sqzer.dev` moves from GoDaddy parking to GitHub Pages: the apex on the four Pages A records, `www` as a CNAME, `CNAME` in the page repository. One Pages site per repository is then no constraint: the page has its own, and docs for the crates, if they ever want a site, get a subdomain from this one. Pages sends no custom headers, which is fine for a single-threaded page and is one more reason threads are out.

---

## 3. Options considered

**One package per codec, as jSquash does.** A page loads only what it needs. Rejected: the point of `sqzer` is one set of defaults over every format, and the content-aware default format needs every encoder present. Feature subsets on one package are the fallback if size matters.

**`wasm-bindgen` CLI and a hand-written `package.json`.** One tool fewer. Rejected: `wasm-pack` keeps the CLI version matched to `Cargo.lock`, which is the failure that bites every few months otherwise, and writes the package metadata from `Cargo.toml`.

**`--target bundler`.** Rejected: it needs a bundler that understands wasm imports; `web` output works in Vite, webpack and a bare `<script type="module">` alike.

**Threads through `wasm-bindgen-rayon`.** Rejected: nightly, `build-std`, and isolation headers the host cannot send. The single-threaded search on a 12 megapixel photo is 18 seconds; the page mitigates with a width, not with threads.

**The browser's own decoders through `createImageBitmap`, everywhere.** Free decoders, HEIC on Safari included. Rejected inside the search loop: decoding is asynchronous and the search calls the decoder inside a synchronous loop, so it needs JSPI, which `wasm-bindgen` calls experimental; and the browser returns 8-bit sRGB only, which would score a different image than the encoder wrote. Accepted for input, as D3's fallback, where being asynchronous costs nothing and where the package has no decoder or, for SVG, a worse one.

**`resvg` on wasm32 with fonts loaded at run time.** `fontdb::load_font_data` works there, and `queryLocalFonts()` hands over installed font files. Rejected: that API is Chromium only, HTTPS, behind a permission prompt, and main thread only; every other browser would need fetched web fonts, with no answer for CJK. The browser's canvas has every font the user has, in every browser, without a prompt. A `loadFonts` entry can come later if a consumer wants output identical to the CLI.

**A `miniz_oxide` backend for `oxipng`.** A hundred-line patch upstream would keep the wasm32 build C-free. Rejected as the plan: upstream deleted that backend in 7.0.0 for ratio and speed and considers `freestanding` its wasm answer, so acceptance is uncertain, and D1 makes it unnecessary. It can still be offered upstream.

**`rusty_av1d`.** Compiles on wasm32 today. Rejected: a fork from an unknown owner with no visible provenance, as recorded in September.

**Keep AVIF encode-only on wasm32.** Rejected: the gap was allowed by ADR-0001 D6 when the fix was unknown; it is 15 lines now.

**Cloudflare Pages.** Custom headers, so threads would be possible later. Rejected for now: one more account and deploy path for a benefit D4 rules out.

**The page under `web/` in this repository.** A wasm API change and the page that uses it in one pull request, the fixtures next door, the CI job loading the page against the package it just built. Rejected: the page becomes AGPL with the full build of `ROADMAP.md`, which this repository cannot hold, and once the page depends on the published package the same-commit convenience is not needed.

---

## 4. Trade-offs

- A C compiler for wasm32 builds. Anyone depending on `sqzer` and targeting wasm32 themselves needs `clang`. Desktop users need a C compiler for the same crate already, so this is documentation, not a new class of requirement.
- The wasm job no longer proves the browser build is C-free, only that it builds without a libc. A vendored C dependency that compiles freestanding gets in unnoticed by CI; the licence and dependency rules of the PR template are what catch it.
- A fork to carry, even a 15-line one, until upstream releases. The alternative was depending on an archived fork of an older upstream.
- 1.9 MB over the wire before the first image. A lean package would be a third of that.
- One thread. Large images are slow on the page; the width control is the mitigation.
- `wasm-pack` in CI, a second toolchain to keep current.
- Two repositories for one feature. A change to the wasm API and to the page that uses it is two pull requests, and the page can lag a release by the version bump.

---

## 5. Consequences

- ADR-0002's wasm32 gate is superseded; the record carries a line saying so.
- `sqzer-codecs`: `sqzer-rav1d` on every target, `oxipng` on every target with `freestanding` on wasm32, the `png` encoder module deleted, AVIF probe and decode registered everywhere.
- `sqzer-wasm`: the D3 surface, `decodeAny` as a `wasm-bindgen` JS snippet so the fallback ships with the package, `fromPixels`, `cdylib`, `wasm-bindgen`, `js-sys`, `serde`, `serde-wasm-bindgen`, `tsify-next`, `console_error_panic_hook`.
- `.cargo/config.toml` with `simd128` for wasm32; `ci.yml`'s wasm job gains `wasm-pack test --node` over the fixtures; `dist-workspace.toml`, `publish-npm.yml`.
- README, `CONTRIBUTING.md`, `CLAUDE.md`: "pure Rust" and "C-free" become the D1 wording; the AVIF and `oxipng` notes are rewritten; the layout entry for `crates/sqzer-wasm`, and a link to the page and its repository.
- `CHANGELOG.md` when the code lands: `codecs` entries for AVIF decoding and `oxipng` on wasm32 and for the `rav1d` crate change, a `wasm` entry for the package.
- The Releases bullet of `CLAUDE.md` gains a step after the tag: bump the version `sqzer-dev/sqzer.dev` imports, or merge the Dependabot pull request that does.
- The `ROADMAP.md` 0.3 note about the browser build's gaps shrinks to lossy WebP and JPEG XL, both licence gaps. SVG comes from the browser with the user's fonts, which is better than the CLI does on wasm32.

---

## 6. Action items

1. [ ] `github.com/sqzer-dev/rav1d`: branch `sqzer` on upstream `main` with the shim, published as `sqzer-rav1d`; the same shim as a pull request upstream. `sqzer-codecs` on it for every target; the `paste` ignore dropped if it can be; the AVIF fixtures decoded in the wasm test.
2. [ ] `oxipng` on wasm32 with `freestanding`, the `png` encoder removed, the golden PNG test running on wasm32 too; `.cargo/config.toml`.
3. [ ] The `sqzer-wasm` surface of D3 with `wasm-pack test --node` over the fixtures: every raster format in and out, an `EncoderUnavailable` for JPEG XL with `availableIn`, `onTrial` called once per trial, `maxPixels` refusing a 25 megapixel header, `fromPixels` round-tripping a fixture's pixels. `decodeAny` cannot run in Node; it is checked in Chrome, Firefox and Safari with item 5, including `createImageBitmap` on an SVG blob inside a Worker, which may need the main thread in some of them.
4. [ ] `publish-npm.yml`, the `dist` config and `dist generate`; the trusted publisher on npmjs.com; a rehearsal with `pr-run-mode = "upload"` on a branch, then the first publish with the 0.3.0 tag.
5. [ ] `github.com/sqzer-dev/sqzer.dev`: the page, its Pages workflow, `CNAME`, Dependabot on `sqzer`, DNS for `sqzer.dev`; this README pointing at the page and the version bump in the Releases bullet of `CLAUDE.md`.
6. [ ] The wording changes of section 5 in README, `CONTRIBUTING.md`, `CLAUDE.md` and `ROADMAP.md`.
7. [ ] Measure `wasm-opt -O` on the search and the AVIF encode; enable it in `[package.metadata.wasm-pack]` only if it is faster.

---

## Sources

- [wasm-bindgen releases](https://github.com/wasm-bindgen/wasm-bindgen/releases): 0.2.128 on 2026-09-05, 0.2.129 on 2026-09-25
- [wasm-pack](https://github.com/wasm-bindgen/wasm-pack) and its [releases](https://github.com/wasm-bindgen/wasm-pack/releases): 0.14.0 on 2026-01-20, 0.15.0 on 2026-05-15; [`Cargo.toml` configuration](https://github.com/wasm-bindgen/wasm-pack/blob/master/docs/src/cargo-toml-configuration.md), `wasm-opt` off by default in release
- [wasm-bindgen-rayon](https://github.com/RReverser/wasm-bindgen-rayon): nightly, `build-std`, cross-origin isolation
- [rayon-core, global fallback when threading is unsupported](https://docs.rs/rayon-core/latest/rayon_core/#global-fallback-when-threading-is-unsupported)
- [memorysafety/rav1d PR 1439](https://github.com/memorysafety/rav1d/pull/1439), the Rust API, merged 2026-04-04; [rav1d on crates.io](https://crates.io/crates/rav1d), 1.1.0 of 2025-05-07; [rerun-io/re_rav1d](https://github.com/rerun-io/re_rav1d), archived 2026-06-09
- [oxipng PR 457](https://github.com/oxipng/oxipng/pull/457), `libdeflater` as the only backend and the `freestanding` feature; [oxipng issue 451](https://github.com/oxipng/oxipng/issues/451), the wasm32 build without headers
- [GitHub `ubuntu-24.04` runner image](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md): clang 16, 17, 18
- [cargo-dist: customising GitHub Actions](https://axodotdev.github.io/cargo-dist/book/ci/customizing.html), custom publish jobs
- [npm trusted publishing](https://docs.npmjs.com/trusted-publishers): OIDC, `--provenance`, npm 11.5.1
- [jSquash](https://github.com/jamsinclair/jSquash) and its package sizes on jsDelivr; [Squoosh](https://github.com/GoogleChromeLabs/squoosh)
- [`sqzer` on npm](https://www.npmjs.com/package/sqzer), the 0.0.1 placeholder
