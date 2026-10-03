# ADR-0012: Palette reduction for PNG

**Status:** Accepted
**Date:** 2026-10-03
**Deciders:** Vlad (sole maintainer)
**Scope:** The first item of `ROADMAP.md`: `-x png:colors=<N>`, what `rimage` users had as `--quantization`. Decides which crate quantises, the two options, where the step runs, and how an option that gives up pixels on a lossless encoder is declared and reported. The default path is untouched: a PNG written without the option is lossless as before.

---

## 1. Context

`rimage` reduces a PNG to a palette with `--quantization` and `--dithering`, through `imagequant`. That crate is GPL-3.0, which ADR-0001 keeps out of the library and the default binaries. ADR-0003 reserved `--codec-opt png:colors=` for the replacement and the README has carried "once a quantiser lands" since. The project does not write a quantiser of its own, so the question is which permissive crate is good enough.

The candidates on crates.io on 2026-10-03:

```text
quantizr 1.4.3      MIT, as a licence file    RGBA, a dithering level, no dependencies, released 2025-04-18
quantette 0.6.0     MIT OR Apache-2.0         RGB only (alpha is its open issue 28), Wu or k-means in Oklab,
                                              Floyd-Steinberg, 25 crates below it with default features off,
                                              released 2026-05-15
exoquant 0.2.0      MIT                       RGBA, k-means, Floyd-Steinberg, no dependencies, released
                                              2016-09-04 and not since
color_quant 2.0.0   MIT                       RGBA, NeuQuant, no dithering
imagequant 4.4.1    GPL-3.0-or-later          the reference: what `pngquant` and `rimage` use
zenquant 0.1.3      AGPL-3.0-only             imazen, or a commercial licence
```

Each permissive one was measured against `imagequant`, in a scratch crate outside the repository. Every candidate quantises with full dithering where it has any, the result is scored with SSIMULACRA2 against the original through `sqzer-metrics`, and `OxipngEncoder` writes it at the default effort. The times are not like for like: `imagequant` and `quantette` ran on 12 threads, the others on one.

Twelve 512 x 512 images of `CID22`, mixed content, 256 colours:

```text
  imagequant (ref)   score  81.77    128107 bytes       66 ms
  quantette wu       score  79.92    130565 bytes        8 ms
  quantette kmeans   score  84.61    130204 bytes       59 ms
  quantizr           score  80.79    126546 bytes       50 ms
  exoquant           score  78.68    135856 bytes      483 ms
  color_quant        score  71.46    125165 bytes       37 ms
```

Ten screenshots of `gb82-sc`, 256 colours:

```text
  imagequant (ref)   score  92.80    140091 bytes      157 ms
  quantette wu       score  84.56    162903 bytes      117 ms
  quantette kmeans   score  91.40    127317 bytes      368 ms
  quantizr           score  92.34    131138 bytes      138 ms
  exoquant           score  81.62    232181 bytes     1281 ms
  color_quant        score  86.29    128013 bytes      274 ms
```

Four photographs of `clic2025/training`, 2048 px on the long edge, 256 colours:

```text
  imagequant (ref)   score  82.56   1495395 bytes      279 ms
  quantette wu       score  80.68   1277264 bytes       91 ms
  quantette kmeans   score  86.32   1443251 bytes      397 ms
  quantizr           score  83.17   1434860 bytes      304 ms
  exoquant           score  83.20   1526724 bytes     2021 ms
  color_quant        score  77.18   1470059 bytes      371 ms
```

`quantette` with k-means and `quantizr` are the two that stay with `imagequant`. `quantette`'s Wu mode is fast and falls apart on flat graphics: 55.75 on a pie chart where `imagequant` scores 91.47.

Three images with alpha, made with `magick` for this: a photograph under a radial fade, a logo with a soft shadow, a screenshot at 60 % opacity. Scored over mid grey, 256 colours. `quantette` has no alpha and is absent:

```text
                     photo, radial fade   logo, soft shadow   screenshot, 60 %
  imagequant (ref)   70.32   52915 B      97.00   11402 B     96.54   20786 B
  quantizr           71.64   63603 B      96.58    9425 B     96.95   22610 B
  exoquant           60.14   62760 B      83.14   12749 B     78.61   30499 B
  color_quant        56.93   64874 B      91.14    9810 B     94.47   19985 B
```

Smaller palettes, the same twelve images and ten screenshots:

```text
                     CID22, 64          CID22, 16          screenshots, 64     screenshots, 16
  imagequant (ref)   68.11   89485 B    34.97   55234 B    86.67   108071 B    69.12    82950 B
  quantette kmeans   71.68   94268 B    39.29   57448 B    86.19   120832 B    70.55   106316 B
  quantizr           64.58   86620 B    28.27   51910 B    84.50   101053 B    62.15    82363 B
```

`quantizr` falls behind as the palette shrinks, by 2 to 7 points, with smaller files.

What else was checked:

```text
wasm32      `quantizr` and `quantette` both build for `wasm32-unknown-unknown` with no C compiler
            and run in Node
repeat      two runs of `quantizr` on one input give the same bytes
licence     `cargo deny check licenses` under this repository's `deny.toml` accepts `quantizr`,
            with a `no-license-field` warning: the manifest names a licence file, the MIT text,
            and no SPDX expression
elsewhere   libvips takes `quantizr` as its substitute for `libimagequant`
```

## 2. Decision

### D1. `quantizr`, in the portable tier

`quantizr` 1.4.3, MIT, becomes a dependency of `sqzer-codecs` under the `png` feature, on every target. Its `capi` feature stays off, so no C entry point is exported. It is the one permissive quantiser that handles alpha at the reference's level, and it brings no crate of its own.

`deny.toml` gains a `[[licenses.clarify]]` entry for it: `MIT`, tied to the hash of its `LICENSE` file. `cargo deny` already accepts the crate from that file, so this loosens nothing. It pins the text, and a release with a different file fails the check.

### D2. Two options

```text
png:colors=<2..=256>    reduce to a palette of at most this many colours.
                        not set: lossless output, as today
png:dither=<0..=100>    how much of the quantisation error is diffused to neighbouring pixels,
                        0 for none. default 100. needs `png:colors`
```

```sh
# rimage oxipng --quantization 75 --dithering 75 in.png
sqzer in.png -f png -x png:colors=256 -x png:dither=75
```

> **Note**: `rimage`'s `--quantization` takes a quality, 1 to 100, and `imagequant` picks the number of colours for it. `quantizr` has no such mode, and a colour count is what ADR-0003 reserved. 256, the most a palette holds, is the nearest equivalent.

The default for `png:dither` is full dithering, which is `pngquant`'s default too. `quantizr` over the three sets above, by dithering level:

```text
colours 256 dither 0:    CID22 78.55  112216 B    screenshots 92.14  120263 B    alpha 85.05  25662 B
colours 256 dither 50:   CID22 80.16  119634 B    screenshots 92.21  126267 B    alpha 87.46  29466 B
colours 256 dither 75:   CID22 80.58  123565 B    screenshots 92.37  128692 B    alpha 88.20  30991 B
colours 256 dither 100:  CID22 80.79  126546 B    screenshots 92.34  131138 B    alpha 88.39  31879 B
colours 32 dither 0:     CID22 41.19   55170 B    screenshots 73.89   77181 B    alpha 55.07  12593 B
colours 32 dither 50:    CID22 48.46   63164 B    screenshots 76.38   82062 B    alpha 66.33  16331 B
colours 32 dither 75:    CID22 50.79   65665 B    screenshots 77.87   84606 B    alpha 69.73  17826 B
colours 32 dither 100:   CID22 52.12   67184 B    screenshots 78.91   87784 B    alpha 71.09  18739 B
```

Full dithering scores highest in every column but one, where it is 0.03 behind. At 256 colours on screenshots it buys 0.2 points for 9 % more bytes; at 32 colours it is worth 5 to 16 points. It is the default because a small palette is where the option is used hardest.

### D3. The step runs in the PNG encoder, on 8-bit RGBA

- It is the first thing `OxipngEncoder::encode` does when `png:colors` is set. Colour conversion, resize and padding have happened by then, so the palette is chosen for the pixels that are written.
- The image goes to `quantizr` as 8-bit RGBA and comes back as a palette and one index per pixel, which `oxipng` receives as an indexed image. Its bit-depth reduction and filter search run as for any input.
- An image that already has at most `N` distinct colours is not quantised. The colours are counted on the samples as they arrive, 16-bit ones at 16 bits, and the image is written as it is, at its own depth. `oxipng` turns it into a palette where one fits.
- Any other image is quantised from 8-bit RGBA: 16-bit samples are narrowed first, since a palette entry is 8-bit, and gray and gray with alpha are expanded. The narrowing is part of the lossy step, never of the exact path above.
- ICC and metadata are handled as before. The palette entries are in the image's colour space.
- `quantizr` runs on the calling thread and spawns nothing.
- A value outside the range, or `png:dither` without `png:colors`, is `Error::InvalidParams` naming the option as the user wrote it.

### D4. An encoder says when its options make it lossy

Until now an encoder with `caps.lossy` false wrote lossless output whatever its options said, and the facade, the CLI record and the npm package all reported `lossless: true` on that basis. `png:colors` breaks the assumption. `png:optimize_alpha` has been breaking it quietly: it changes the colour under fully transparent pixels, and 0.3 reports such a run as lossless. The encoder is the one that can tell, so it is asked, and the output carries the answer:

```rust
pub trait Encoder {
    // ...
    /// Whether an encode at a lossless target keeps every sample it is given
    /// under `params`. `false` when an option that gives samples up is set.
    fn exact(&self, params: &EncodeParams) -> bool {
        true
    }
}

pub struct Output {
    // ...
    /// The bytes hold exactly the samples the encoder was given: the target
    /// resolved to lossless and the encoder is exact under its options.
    pub lossless: bool,
}
```

`OxipngEncoder` answers `false` when `png:colors` is set or `png:optimize_alpha` is true. Every other encoder keeps the default. The remaining options of the encoders with a lossless mode are `png:interlace`, `webp:predictor` and `jxl:container`, which change how samples are stored and not the samples, and `webp:alpha_quality` and `webp:sharp_yuv` of the native backend, which `libwebp` reads for lossy output only. The last two are taken from its documentation, not measured; the test of item 2 holds all five to it.

```text
default target, a lossy option set      one encode, no search. `lossless` is false,
                                        there is no quality and no score
`--lossless` or `--preset lossless`     refused, `Error::InvalidParams`: the two contradict.
  with a lossy option                   the CLI refuses before any file is touched when
                                        `-f` names the format, and at the image otherwise
`-q` with `-f png`                      refused as today. PNG has no quality scale,
                                        and `caps.lossy` stays false
```

The `lossless` field of the CLI's JSON record and of the npm package's result reads `Output::lossless`. The help line of a lossy option says so, which is what `--list-codecs -v` and the package's `codecs()` print. `{quality}` in an output template renders `lossy` for such a run, where it renders `lossless` for the same encoder without the option.

> **Note**: this changes two things for `png:optimize_alpha=true`, which 0.3 accepts: with `--lossless` it is now refused, and without it the record says `lossless: false`. The bytes written are the same as before.

A later option that gives samples up needs its encoder's `exact` to say so, and nothing in the CLI, the records or the facade.

### D5. Not decided here

- A search over the palette size towards the SSIMULACRA2 target, `png:colors=auto`. It would bring PNG under the perceptual default, and needs the search loop to move along something other than a quality. Its own record if it is asked for.
- Scoring the output of a fixed palette, so the record says what the reduction cost.

## 3. Options considered

**`quantette` for opaque images, `quantizr` for the rest.** `quantette`'s k-means scores 3 to 4 points over `quantizr` on photographs. Rejected: two quantisers and 25 more crates for one option, and the character of the output would change with a single translucent pixel. Measure again if `quantette` gains alpha.

**`quantette` alone, refusing images with alpha.** Rejected: logos, icons and interface captures are where a palette PNG earns its place, and they are the images with alpha.

**`imagequant` in `sqzer-codecs-agpl`.** GPL-3.0 is compatible with that crate's licence, and `ROADMAP.md` already lists it for the AGPL browser build. That is where it goes, with that build's record. It does not answer `rimage` users of the default binaries.

**A top-level `--colors` flag.** Rejected: a knob one backend has goes through `--codec-opt` (ADR-0003), and `EncodeParams` takes no field for it.

**A `lossy` flag on `CodecOption` in place of the method.** Static, so the listings could show it as a column. Rejected: a flag on the key cannot see the value, so `png:optimize_alpha=false` would count as lossy, and it adds a field to a struct every backend builds. The backend already parses its own values; `exact` reuses that.

**`caps.lossy` set for `oxipng`.** Rejected: `lossy` means the encoder has a quality scale the search can move along. `-q 80 -f png` would be accepted and mean nothing.

## 4. Trade-offs

- `quantizr` is behind `imagequant` on small palettes: 28.3 against 35.0 at 16 colours on `CID22`, 62.2 against 69.1 on screenshots. At 256 colours it is within about a point either way.
- One maintainer, and the last release is from April 2025. It is about 1500 lines with no dependencies, so there is little in it to rot, and the golden test catches a release that changes the output.
- No SPDX expression in its manifest. The `clarify` entry covers it, and has to be touched on a release that edits the licence file.
- `Output` gains a public field. A caller that builds one by hand breaks. `Encoder::exact` has a default, so a backend outside the workspace keeps compiling.
- `--lossless -x png:optimize_alpha=true` stops working. The same output is one flag away: PNG is lossless-only, so dropping `--lossless` writes it.
- `lossless: false` on an image that had at most `N` colours to begin with, or no transparent pixel for `png:optimize_alpha` to touch, where the output is in fact exact. `lossless` promises, it does not measure.
- The npm package grows by the quantiser. Not measured yet; item 4.

## 5. Consequences

- The README's `rimage` mapping loses "once a quantiser lands" and gains `png:dither`. `ROADMAP.md` loses its first section.
- `CHANGELOG.md` gets the feature, and under breaking changes the new field of `Output` and what changes for `png:optimize_alpha`.
- ADR-0008 item 3 applies: the encode holds an RGBA copy, the indices and the quantiser's histogram on top of what the PNG encoder held.
- The `no-banned-crates` rule of `.greptile/config.json` names `imagequant`, `imagequant-sys` and `zenquant`. `cargo deny` refuses all three by licence already.
- The golden tables of `crates/sqzer/tests/golden.rs` and `crates/sqzer-wasm/src/tests.rs` each gain the palette rows.

## 6. Action items

1. [ ] `quantizr` in `sqzer-codecs` under `png`, the `clarify` entry in `deny.toml`, `png:colors` and `png:dither` in `OxipngEncoder`. Tests: the colour count is respected at 256 and 16 on an RGB and an RGBA fixture, alpha survives, an image with fewer colours comes back exact at 8 and at 16 bits, a 16-bit image with more colours is quantised, the refused values.
2. [ ] `Encoder::exact` and `Output::lossless`, the refusal under a lossless target in the facade and in the CLI, the two records, the help lines, the `{quality}` word. The test that `exact` is truthful: with `png:colors` or `png:optimize_alpha` the samples change and it says `false`; with every other option of every encoder that has a lossless mode they round-trip and it says `true`.
3. [ ] Golden scores for `png:colors=256` and `png:colors=16` in both tables. The same bytes from the native build and the wasm32 one, checked by hand once.
4. [ ] The peak memory of a 24 megapixel encode with `png:colors`, by ADR-0008's method, and the estimate adjusted if it is exceeded. The size of the npm package before and after.
5. [ ] README, `ROADMAP.md`, `CHANGELOG.md`.

## Sources

- [`quantizr`](https://github.com/DarthSim/quantizr) and its [`LICENSE`](https://github.com/DarthSim/quantizr/blob/master/LICENSE)
- [`quantette`](https://github.com/IanManske/quantette) and its [issue 28, alpha support](https://github.com/IanManske/quantette/issues/28)
- [`exoquant`](https://github.com/exoticorn/exoquant-rs), [`color_quant`](https://github.com/image-rs/color_quant), [`imagequant`](https://github.com/ImageOptim/libimagequant), [`zenquant`](https://github.com/imazen/zenquant)
- [libvips `quantise.c`](https://github.com/libvips/libvips/blob/master/libvips/foreign/quantise.c), `HAVE_QUANTIZR`
- [`cargo deny` licence clarification](https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html#the-clarify-field-optional)
- [pngquant man page](https://github.com/kornelski/pngquant/blob/main/pngquant.1)
- The corpora are `codec-corpus`'s, as `tools/calibrate` downloads them: `CID22/CID22-512`, `gb82-sc`, `clic2025/training`
