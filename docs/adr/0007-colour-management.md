# ADR-0007: Colour management through `moxcms`

**Status:** Accepted
**Date:** 2026-09-22
**Deciders:** Vlad (sole maintainer)
**Scope:** Which colour management system does the ICC to sRGB conversion of ADR-0001 D3 and D7, where in the pipeline it runs, what the default and `--keep-icc` do to the samples and the profile bytes, and what the metric sees. Settles the `qcms` or `lcms2` question D3 left open. Nothing here changes D7's policy: convert to sRGB and drop the profile by default, keep it untouched on request.

---

## 1. Context

ADR-0001 D3 named two candidates and a condition: `qcms`, Mozilla's pure-Rust CMS, pending a maintenance check, or `lcms2`, a C binding. The pipeline has since fixed what the stage has to accept: the `Image` type's `u8`, `u16` and `f32` samples in `Gray`, `GrayAlpha`, `Rgb` and `Rgba` layouts, on every target including `wasm32-unknown-unknown`, in the portable tier, because the default policy is to convert and a portable build that cannot convert would have to strip profiles silently, the colour shift `rimage` users reported (ADR-0003's survey, issue 138).

What the three crates are, checked on 2026-09-22:

```text
qcms 0.3.0      MIT. Last release 2024-01-09, last push to the repository 2025-03-05.
                8-bit only: `DataType` is RGB8, RGBA8, BGRA8, Gray8, GrayA8, CMYK. No 16-bit,
                no float. Unsafe throughout, SIMD gated on x86 and arm.
lcms2 6.2.0     MIT binding over Little CMS (MIT), released 2026-08-26, maintained. Every depth
                and layout. A C library: a compiler at build time, native tier only, no wasm32.
moxcms 0.9.1    BSD-3-Clause OR Apache-2.0, pure Rust, released 2026-09-15, pushed 2026-09-17,
                one maintainer, 82 million downloads. 8, 10, 12 and 16-bit, `f32` and `f64`
                transforms; `Gray`, `GrayAlpha`, `Rgb` and `Rgba` layouts; matrix-shaper and
                LUT profiles, CICP, ICC v2 and v4; `forbid(unsafe_code)` outside its SIMD
                feature paths. Already in this workspace's lock file at 0.8.1 through
                `image` 0.25.10, which depends on `^0.8`, so `sqzer-wasm` builds it today.
```

The maintenance check D3 asked for came back negative for `qcms`, and its 8-bit limit would have meant widening `u16` and `f32` sources down before conversion, which discards what the deeper decoders preserve.

## 2. Decision

### D1. `moxcms`, in the facade, in the portable tier

`moxcms` at `0.8`, the same requirement `image` carries, so the lock file holds one copy. It lives in `crates/sqzer/src/color.rs` next to the resize adapter: a stage of the pipeline, not a codec, so not in `sqzer-codecs`, and not in `sqzer-core`, which stays dependency-free. No feature flag: the conversion is the default behaviour and every build has it.

### D2. Order: orient, colour, resize, encode

ADR-0001 D3 wrote the stages as `[orient, resize, color-manage]`. The implemented order is orientation in the decoder, then colour, then resize. The resize linearises integer samples with the sRGB curve, so it is exact only after the conversion; the metric assumes sRGB for the same reason. Converting first also means the resampler works on data in one known encoding whatever the source was.

### D3. What the stage does

With a profile on the image and `keep_icc` false: convert the samples to sRGB with the profile's own rendering intent, drop the profile. What the encoder gets is untagged sRGB, which every viewer reads as sRGB, so the picture looks the same as the tagged original and the bytes for the profile are saved.

```text
profile colour space   image layout        action
RGB                    Rgb, Rgba           convert through moxcms, alpha carried through untouched
Gray                   Gray, GrayAlpha     convert gray to gray against an sRGB-curve gray profile
RGB on Gray, Gray on RGB, anything else    no defined conversion: the profile is dropped, the
                                           samples are read as sRGB
unparseable profile                        `Error::Transform { stage: "color" }`, never a pass-through
```

Sample types: `u8` and `u16` go through the 8-bit and 16-bit transforms in their own depth. `f32` samples are linear light by the `Image` contract, so a tone curve cannot apply to them; a matrix-shaper profile contributes its primaries and adaptation as one `RGB -> XYZ -> RGB` matrix, applied in linear space, negative results clamped to zero and values above one kept. A LUT profile on float samples is refused as `Error::Transform`. No decoder produces `f32` with a profile today; OpenEXR (ADR-0001 D7) will not either, since EXR carries chromaticities, not ICC.

No sRGB shortcut: a profile that happens to describe sRGB goes through the transform like any other. The transform is an identity to within one 8-bit step on such a profile, checked by a test; a detection heuristic would be a second thing to get wrong.

With `keep_icc` true: nothing is touched. The samples stay in the profile's encoding, the profile bytes go to the encoder as they came out of the decoder, and an encoder that cannot embed a profile refuses (both AVIF backends), the existing behaviour.

### D4. The metric

`sqzer-metrics` keeps reading `u8` and `u16` as sRGB. By default that is now exact. Under `keep_icc` the score is computed in the profile's encoding, an approximation that is the same on both sides of the comparison; the rustdoc says so.

## 3. Options considered

**`qcms`.** Rejected on the facts above: two years without a release, 8-bit only.

**`lcms2`.** The reference implementation, and the widest profile support. Rejected as the answer for the default path because it is a C library, which puts it in the native tier and leaves the portable tier and the browser package without the conversion. It stays a candidate for a `native-cms` feature if a class of profile turns up that `moxcms` handles worse; nothing in the stage's interface would change.

**Strip the profile without converting.** What most optimisers do. Rejected: it is the colour shift the survey recorded, and the one behaviour D7 was written to rule out.

**Convert inside each decoder.** Rejected: five decoders keep a profile today and every new one would have to repeat the policy; one stage in the facade means one place for the rule and one place for `keep_icc`.

## 4. Trade-offs

A crate under a year old with one maintainer, against a stalled one and a C binding. The mitigation is the interface: the stage is one function over `Image`, and `moxcms` is not visible outside `color.rs`. The version is tied to `image`'s so that a bump there is a bump here, in the same PR.

Every tagged image pays one full-frame transform, including sRGB-tagged JPEGs from cameras. Measured on a 2000 x 1500 PNG with the release binary: 188 ms for decode and JPEG encode untagged, 204 ms with a Display P3 tag, so the stage costs about 15 ms on 3 megapixels, under a tenth of the untagged run. If that ever matters, the shortcut in D3 is the lever.

## 5. Consequences

- `Sqzer::transform` now does colour then resize; `EncodeParams::keep_icc` gains its meaning. No new builder methods, no new CLI flags.
- Encoders see an ICC profile only under `keep_icc`. The `ravif` and `libavif` refusals stay as they are.
- `Error::Transform` gains the `color` stage.
- The `wasm32` build carries the conversion without a size cost that was not already paid through `image`.

## 6. Action items

1. [x] `color.rs` with the table of D3, wired into `Sqzer::transform` before the resize; tests for the P3 fixtures in every container that carries one, neutrals unchanged, saturation up, `keep_icc` bytes and samples untouched, the sRGB identity bound, the gray path, the mismatch and the unparseable profile.
2. [x] Cross-check the P3 conversion against Little CMS once, through ImageMagick on the fixtures; the numbers go in the PR, not in a test.
3. [ ] A gray fixture with a gray profile, once a tool that writes one is at hand; the gray path is tested on a synthesised profile until then.
4. [ ] If `image` moves to `moxcms` 0.9 or later, move with it in the same PR.

---

## Sources

- [qcms on crates.io](https://crates.io/crates/qcms), [FirefoxGraphics/qcms](https://github.com/FirefoxGraphics/qcms)
- [lcms2 on crates.io](https://crates.io/crates/lcms2), [kornelski/rust-lcms2](https://github.com/kornelski/rust-lcms2)
- [moxcms on crates.io](https://crates.io/crates/moxcms), [awxkee/moxcms](https://github.com/awxkee/moxcms)
- [image 0.25.10 manifest](https://github.com/image-rs/image/blob/v0.25.10/Cargo.toml), the `moxcms` requirement
