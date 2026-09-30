# ADR-0010: Stage types, `--fit outside` and translucent padding

**Status:** Accepted
**Date:** 2026-09-28
**Deciders:** Vlad (sole maintainer)
**Scope:** Replaces ADR-0009 D4's `Sqzer::prepare` and `Sqzer::resize` methods with a type per stage, adds a fifth fit, and settles what an encoder without alpha does with a translucent `--background`. Everything else in ADR-0009 stands: the flags of D1, the fits of D2, the width list of D3, the `Resize` type and its geometry in `sqzer-core`.

---

## 1. Context

ADR-0009 D4 split `Sqzer::transform` into `prepare(Decoded) -> Decoded` and `resize(&Decoded, &Resize) -> Decoded`, and put the padding a contain fit still owes on `Decoded` as a `canvas` field. Implementing it (PR #35) showed three problems.

```text
name clash      Sqzer::resize(Resize) -> Self is already the builder setter, so the stage
                became Sqzer::resize_image
order           every stage takes and returns Decoded, so a caller can encode an image
                that was never prepared, or resize one twice
core leak       Decoded lives in sqzer-core, which now carries state only the facade reads
```

`zune-image` avoids the first by making each operation a value that a `Pipeline` runs in order. That fits a general image library, where any order of any operations is legitimate. It does not fit `sqzer`, whose stage order is fixed (ADR-0007 D2), and whose width list is a tree, one prepare fanning out to several resizes and each to several encodes, not a chain.

Separately, `rimage` users asked for resizing by the shortest side (`rimage#357`), for batches that mix portrait and landscape. `rimage` shipped it as `--resize 1000s`. ADR-0009 had no equivalent. And the PR review found that an explicit `--background transparent` with `-f jpeg` was made opaque without a word, against the rule that a setting an encoder cannot honour is an error.

## 2. Decision

### D1. A type per stage

```rust
// once per input: colour, range, metadata, as ADR-0009 D4's prepare
pub fn Sqzer::prepare(&self, decoded: Decoded) -> Result<Prepared>;
// once per size, from a borrowed prepared image
pub fn Prepared::resize(&self, resize: &Resize) -> Result<Ready>;
// prepare, then the builder's resize, without copying the source
pub fn Sqzer::transform(&self, decoded: Decoded) -> Result<Ready>;
// pads a pending canvas, then encodes
pub fn Sqzer::encode(&self, ready: &Ready) -> Result<Output>;
```

`Prepared` and `Ready` live in the `sqzer` facade, with private fields and read accessors. The operation sits on the value it transforms, so `Prepared::resize` and the setter `Sqzer::resize` no longer share a type. `encode` takes only a `Ready`, so the compiler enforces the order of ADR-0007 D2. The pending canvas is a field of `Ready`, and `Decoded` in `sqzer-core` goes back to an image and its format. `Prepared` carries the pixel limit it was prepared under, so a resize still refuses an output over `max_pixels`.

A caller with pixels of its own wraps them in a `Decoded` and calls `prepare`, the way the decoders' output does.

### D2. `--fit outside`

```text
outside    scale to cover the box, keep the aspect ratio, crop nothing: both sides end
           at or beyond the box. needs both --width and --height
```

It is `sharp`'s fifth fit. With a square box it is `rimage`'s shortest side:

```sh
# rimage --resize 1000s
sqzer photo.jpg --width 1000 --height 1000 --fit outside
```

Without `--enlarge` the scale is capped at one, like every other fit. `--position` and `--background` have no meaning for it and are argument errors, as for `inside` and `fill`.

### D3. A translucent background needs an encoder with alpha

A `--background` whose alpha is below 255, on an encoder without alpha, is `Error::Unsupported`. The CLI refuses it before any file is touched when `-f` names such a format (exit 3). The default format treats an explicit translucent background as alpha, so it picks a format that can carry it. The default background of ADR-0009 D2, transparent or white by encoder, is unchanged: it is a default, not a request.

## 3. Options considered

**A `zune-image` style `Pipeline`.** A list of boxed operations run in order. Rejected: the order is fixed here, so a free list only adds ways to get it wrong, and the width list would need a branching step the linear pipeline does not have.

**Keep ADR-0009 D4 and rename the setter.** `Sqzer::with_resize` for the builder frees `resize` for the stage. Rejected: it breaks the setter's naming against every other setter (`format`, `target`, `effort`), and the order and core-leak problems remain.

**A dedicated `--shortest-side` flag.** Rejected: `outside` keeps to the CSS and `sharp` vocabulary ADR-0009 chose, and the geometry is cover's scale without the crop.

## 4. Trade-offs

- Two public types instead of one. A caller of `transform` then `encode` does not notice; a caller that encoded a raw `Decoded` now calls `prepare` first.
- Another breaking change to the library, taken in the same release as ADR-0009's so it breaks once.

## 5. Consequences

- `Decoded::canvas` from ADR-0009 D4 is not added to `sqzer-core`.
- `Sqzer::pick_format` and `Sqzer::encode_with` take a `Ready`.
- The `rimage` hint and the README map `--resize Ns` to `--fit outside`.

## 6. Action items

1. [x] `Prepared` and `Ready` in the facade, `Decoded` back to two fields, the CLI on the new types.
2. [x] `Fit::Outside` in the geometry and the CLI, tested with a square box on both orientations.
3. [x] The translucent-background refusal in `encode`, the dry run and the CLI's format check, and the default format counting it as alpha.

## Sources

- [`sharp` resize `fit` options](https://sharp.pixelplumbing.com/api-resize)
- [`zune-image` `Pipeline`](https://docs.rs/zune-image/0.5.0/zune_image/pipelines/struct.Pipeline.html)
- `rimage#357`, shortest and longest side, and `rimage#377`, which added them
- PR #35 review: transparent padding made opaque for JPEG
