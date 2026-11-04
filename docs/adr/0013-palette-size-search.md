# ADR-0013: Searching the palette size against the target

**Status:** Accepted
**Date:** 2026-11-04
**Deciders:** Vlad (sole maintainer)
**Scope:** Issue [#65](https://github.com/sqzer-dev/sqzer/issues/65) and the first open item of ADR-0012 D5: `-x png:colors=auto`, a palette size searched for the target as the quality is searched for a lossy format. Decides what the search moves along, how it bisects a palette size, how an encoder asks for it, and what the records say. A fixed `png:colors` writes the same bytes as before.

---

## 1. Context

`png:colors=N` takes a fixed palette size, so the user guesses how many colours an image can lose before it shows. The target search answers that question for every lossy encoder, and a palette PNG never searches. Its record said so badly: the npm package at 0.4.0 reported the default `target` for an encode that searched for nothing.

```js
encode({ format: "png", codecOpts: { "png:colors": "16" } });
// { format: "png", backend: "oxipng", lossless: false, target: 70, ... }
// no quality, no score, no trials
```

The search of `sqzer-metrics` bisects a whole number between a floor and a ceiling, on the assumption that the score rises with it, and picks over everything it tried. Nothing in that needs the number to be a quality, except the range check, `0..=100`, and the names of the fields.

The issue asked for a measurement before the shape is settled: whether the score rises with the colour count closely enough for a bisection, dithering especially. Measured on 2026-11-04, in a scratch crate outside the repository, through `OxipngEncoder` at effort 2, which changes the size and not the pixels, and `Reference` of `sqzer-metrics`. 41 images of `CID22/CID22-512/validation` and the 10 screenshots of `gb82-sc`, both at `png:dither=0` and at the default 100, every size from 2 to 16, every second one to 64 and every eighth one to 256: 63 sizes, 6426 encodes.

The score against the colour count, as the largest drop from fewer colours to more along each of the 51 curves:

```text
dither 0:     median 0.23   p90 2.33   max 13.70   over 1 point on 13 of 51
dither 100:   median 0.16   p90 1.19   max 10.22   over 1 point on 6 of 51
```

The drops over a point sit at 5 to 46 colours, 13 of the 19 under 17, and the two largest are one screenshot, `gb82-sc/windows`, at 5 and 9 colours. Those palettes score far under any useful target, so a bisection that brackets a target of 60 or more rarely sees them. The size rises with the colour count less reliably than the score does: more colours gave a file over 2 % smaller at 90 points of the dither 0 grid and at 152 points of the dither 100 grid.

The median score at 256 colours, the most a palette can do:

```text
CID22, dither 0:      75.7
CID22, dither 100:    78.8
gb82-sc, dither 0:    95.2
gb82-sc, dither 100:  95.1
```

Then the search itself, run over those images with a budget of six encodes and a tolerance of 1, three ways: bisecting at the arithmetic midpoint, bisecting at the geometric mean, and the geometric one keeping the smallest file among the trials that reach where the others keep the fewest colours. Each pick is set against the grid: `size` is the chosen file over the smallest file on the grid that reaches the target, `colours` is the chosen size over the fewest colours on the grid that reach it. Every strategy reached every image the grid says is reachable, so only those are counted.

```text
                          reachable   encodes   size mean   size max   colours mean
linear     70  dither 0     45/51       4.45      1.011       1.115       1.049
linear     70  dither 100   48/51       3.98      1.028       1.399       1.090
linear     80  dither 0     26/51       4.63      1.012       1.075       1.059
linear     80  dither 100   30/51       4.47      1.023       1.078       1.112
geometric  70  dither 0     45/51       4.90      1.012       1.082       1.041
geometric  70  dither 100   48/51       4.90      1.020       1.494       1.043
geometric  80  dither 0     26/51       5.02      1.016       1.097       1.055
geometric  80  dither 100   30/51       4.90      1.020       1.101       1.093
geo-size   70  dither 100   48/51       4.90      1.020       1.494       1.043
geo-size   80  dither 100   30/51       4.90      1.020       1.101       1.093
```

At 60 and 90 the picture is the same: every strategy reaches what can be reached, the geometric midpoint picks 1.02 to 1.04 times the fewest colours where the linear one picks 1.05 to 1.06, and spends at most a quarter of an encode more. Keeping the smallest file in place of the fewest colours changes the mean size by at most 0.2 %.

## 2. Decision

### D1. `png:colors=auto` searches the palette size

```text
png:colors=<2..=256>|auto|off   auto: the fewest colours, 2 to 256, that reach the target,
                                searched as the quality is. off: no palette, the default
png:dither=<0..=100>            as before. stays at what it says through the search
```

```sh
# the fewest colours that still score 80, with the default dither
sqzer logo.png -f png -x png:colors=auto --target 80

# the same, with no dithering
sqzer logo.png -f png -x png:colors=auto -x png:dither=0 --target 80
```

The search is the quality search with the palette size in place of the quality: the same target, tolerance and budget of six encodes, the same rule that spends the last encode on the ceiling when nothing has reached, the same pick of the fewest colours among the trials that reach and of the highest score among the rest. A target nothing reaches at 256 colours is capped and reported, not an error. There is no seed: the measurement has no table behind it, and the answer spreads over the whole range with the content.

The dither is not searched. It is a second axis, more trials, and the measurement shows the default of 100 scoring higher than 0 at 256 colours on photographs, as ADR-0012 D2 found.

### D2. A palette size is bisected at the geometric mean

The midpoint of a palette bracket is `round(sqrt(lo * hi))`, so the first trial of a search over `2..=256` is 23. An index costs about the logarithm of the palette size in bits, and the score moves most between small palettes, so equal ratios are equal steps. The quality keeps its arithmetic midpoint.

The measurement does not make this a large win. The geometric midpoint lands 2 to 5 % closer to the fewest colours that reach, writes files 0.3 to 0.9 % smaller on average at the default dither, and spends up to one encode more at target 70. It is the choice because palettes are where the answers are small numbers, logos and interface graphics, and there a linear bisection spends its first four encodes on 129, 66, 34 and 18.

### D3. The encoder names the option, the facade runs the search

```rust
pub trait Encoder {
    // ...
    /// The codec option a perceptual target searches under `params` in
    /// place of a quality: a palette size, `2..=256`, which each trial
    /// writes to this key at a lossless target. `png:colors` under
    /// `png:colors=auto`.
    fn palette_search(&self, params: &EncodeParams) -> Option<&'static str> {
        None
    }
}
```

`OxipngEncoder` answers `Some("png:colors")` when `png:colors` is `auto`. `Sqzer::encode` asks before it picks a mode: with an option named and a perceptual target, it runs `Search::colors` over the encoder, and each trial writes its size to that key over `auto`, at `Target::Lossless`. `Output::target` is `Resolved::Lossless`, PNG's one mode, and `Output::lossless` is `false`: `Encoder::exact` already answers `false` for any `png:colors` but `off`.

```text
perceptual target                 the palette search
explicit quality, lossless        refused, `Error::InvalidParams`: nothing to search for.
target, or fast mode              `Sqzer::palette_search` says so, and the CLI refuses before
                                  any file is touched when `-f` names PNG, at the image otherwise
`auto` reaching `encode`          refused by `OxipngEncoder` itself: there is no number to
                                  quantise to
```

A later encoder with a palette, lossless WebP's for one, implements the method and needs nothing in the facade, the CLI or the records.

### D4. The search moves along an `Axis`

`sqzer-metrics` gains the axis the search moves along, and the names that said quality say value:

```rust
#[non_exhaustive]
pub enum Axis {
    Quality,              // 0..=100, arithmetic midpoint
    Colors(&'static str), // 2..=256, geometric midpoint, written to that codec option
}

pub struct Search   { pub axis: Axis, /* ... */ }
pub struct Trial    { pub value: f32, pub score: f32, pub size: u64 }
pub struct SearchReport { pub axis: Axis, pub value: f32, /* ... */ }
```

`Search::colors(key, target)` is the palette search over the whole range. `Search::validate` checks the floor and ceiling against `Axis::range`. `Progress::Trial` of the facade carries `axis` and `value` in place of `quality`.

### D5. The records name the palette size `colors`

A colour count is not a quality on 0 to 100, so it gets a key of its own. The `--json` record and the npm package's result:

```text
colors                     the palette size the search settled on. palette search only
trials[].colors            the size each trial ran at, in place of `quality`
onTrial({ colors, ... })   the same, as each trial is scored
quality                    left out on a palette search, as for any lossless-only encoder
```

The human line reads `64 colours s80.4` where a quality search reads `q64 s80.4`, and `-v` and the missed-target warning follow. `{quality}` in a `--template` renders `lossy`, as for a fixed palette.

### D6. `target` is reported where something answers to it

`target` in both records is the target that was searched for, or that lossless output meets by its one mode. A fixed `png:colors` searched for nothing and is not exact, so it leaves `target` out. Lossless PNG and lossless WebP keep it, as before.

### D7. Not decided here

- Searching `png:dither` with the size.
- A seed table for the palette size, from `tools/calibrate`. The measurement here is the start of one.
- Scoring the output of a fixed palette, the second item of ADR-0012 D5.

## 3. Options considered

**The colour count in `quality`.** No new key and no rename. Rejected: a palette size is not a quality on 0 to 100, a reader of the record would have to know the format to read the number, and the page would plot it on a quality axis.

**`png:quality=<0..=100>` mapped to a colour count, as `rimage` maps `--quantization`.** The existing quality search would run unchanged. Rejected: `caps.lossy` would have to be true for PNG, `-q 80 -f png` would then mean a palette (ADR-0012 options), and the mapping from a quality to a count is a guess the target search exists to replace.

**Bisecting the palette size at the arithmetic midpoint.** One encode cheaper at the default target, within a percent of the size. Rejected for D2's reason, and kept here because the measurement does not make the margin wide.

**Keeping the smallest file among the trials that reach.** Smaller files where the size falls as the palette grows. Rejected: at most 0.2 % smaller on the corpus, and the quality search keeps the cheapest setting, so the two would differ for nothing.

**A new search type for palettes.** Rejected: it would repeat the bisection, the budget rule and the pick, and the records would need a second report.

## 4. Trade-offs

- `SearchReport`, `Trial`, `Search` and `Progress::Trial` change shape. Code that read `quality` reads `value`. The CLI's `--json` for a quality search is unchanged; the TypeScript type of a trial's `quality` becomes optional.
- The corpus has no image with alpha and no logo. ADR-0012 measured three images with alpha at fixed sizes; nothing here says how the search does on them.
- A photograph rarely reaches 80 through a palette: 26 to 30 of the 51 images reached it at all, and the median CID22 image scores 76 to 79 at 256 colours. The search reports the cap, and AVIF is the format for those.
- `lossless: false` on an image the search settled on at or above its own colour count, where the output is exact. As in ADR-0012, `lossless` promises, it does not measure.
- Six encodes cost six quantisations and six `oxipng` passes. The CLI's memory budget already reserves a search for every perceptual target, so `-j` is unchanged.

## 5. Consequences

- `CHANGELOG.md` gets the feature, the renames under breaking changes, and the `target` of a fixed palette as a fix.
- The README and the package's README show `auto` and the `colors` key.
- The help line of `png:colors` lists `auto`. The page on `sqzer.dev` reads a range in a help as a slider (its ADR-0007 D6), so `auto` needs a control there after a release; that is a change to the page.
- ADR-0012 D5's first item is this record.

## 6. Action items

1. [x] `Encoder::palette_search`, `Axis`, `Search::colors` with the geometric midpoint, `value` in place of `quality`. Tests: the midpoint, the range check, a logarithmic curve, the cap at 256, a flat curve. (Done on 2026-11-04.)
2. [x] `png:colors=auto` in `OxipngEncoder`, refused when it reaches `encode`. The search in the facade, `Sqzer::palette_search` and its refusals, the up-front refusal in the CLI. Tests: a photograph reaches with every trial whole and in range, the bytes are the chosen trial's, a higher target takes no fewer colours, the dither holds, a four-colour graphic settles on four or five. The failure modes of the search through the real encoder in `crates/sqzer/tests/palette_search.rs`: tiny images, a flat image, pure noise, a target out of reach at 256. Pure noise is not a failure for a palette: dithered, it reaches 70 at about 160 colours. (Done on 2026-11-04.)
3. [x] `colors` in the `--json` record, in the npm package's result, its trials and `onTrial`; `target` where D6 says. Tests in `crates/sqzer-cli/tests/cli.rs` and `crates/sqzer-wasm/src/tests.rs`. (Done on 2026-11-04.)
4. [ ] The search on images with alpha and on logos, the three of ADR-0012 and a few icons, and the table of section 1 extended with them.
5. [x] README, the package README, `CHANGELOG.md`. (Done on 2026-11-04, in the same pull request.)

## Sources

- Issue [#65](https://github.com/sqzer-dev/sqzer/issues/65)
- [pngquant manual](https://pngquant.org/#manual), `--quality min-max`
- The corpora are `codec-corpus`'s, as `tools/calibrate` downloads them: `CID22/CID22-512/validation`, `gb82-sc`
