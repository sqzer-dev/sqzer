# Calibration harness

Generates the seed tables in `crates/sqzer-metrics/src/seeds/tables.rs`: for
each lossy backend and each SSIMULACRA2 target, the quality at which the
median corpus image first reaches the target, with the quartiles around it.
The target search starts there instead of at the midpoint of the range
(ADR-0001 D4).

This is a maintenance task, not a build step. The tables are committed as
data and regenerated when a backend changes, when a dependency bump moves
an encoder's quality scale, or when the corpus changes.

## Why it is not a workspace member

The harness builds on `codec-eval` (MIT OR Apache-2.0), which depends on
`dssim-core` (AGPL-3.0). The licence allow-list in `deny.toml` must keep
AGPL out of everything that ships, so this package has its own `[workspace]`
and the root `Cargo.toml` excludes it. Nothing here is linked into a crate;
the output is a table of numbers.

## Running it

Both commands download their datasets through `codec-corpus` on first use
and cache them under the user's cache directory (`~/.cache/codec-corpus` on
Linux; set `CODEC_CORPUS_CACHE` to move it). Nothing from the corpus is ever
committed.

```sh
cd tools/calibrate

# Sweep every lossy backend this build can also decode over the default
# corpus and overwrite the tables. Every core is used; each worker holds
# one source image and one encode, so lower --jobs on a small machine.
cargo run --release -- sweep --jobs 6

# Then rebuild with the new tables and measure them on the held-out split:
# the search seeded from the tables against the unseeded search, per
# backend and target, as mean encodes, first-encode hits, hit rate, mean
# chosen quality and mean output size.
cargo run --release -- verify

# Faster iterations while working on the harness itself.
cargo run --release -- sweep --datasets CID22/CID22-512/training --limit 8 --formats jpeg --out /tmp/tables.rs
```

Per-image reports (`codec-eval`'s JSON, one file per source with every
quality tried, its size and score) land in `target/calibration/<backend>/`.
The sweep prints each table as plain text as well; paste that block into
the PR that updates the tables.

## What is calibrated

Default corpus for `sweep`:

```
CID22/CID22-512/training   209 images, 512 x 512: photos, portraits, text,
                           graphics, plots, medical and scientific imagery
gb82-sc                    10 screenshots and screen content, 640 to 2940 px
clic2025/training          32 photographs, about 2048 px on the long edge
```

Default split for `verify`: `CID22/CID22-512/validation`, 41 images none of
which are in the sweep.

Each source is decoded through `sqzer`'s registry, reduced to RGB8 (alpha
dropped, gray widened) and treated as sRGB with any ICC profile discarded.
Each backend is driven through `sqzer`'s `Encoder` trait at the default
`EncodeParams` (effort 6, automatic chroma subsampling), so the table
measures the backend exactly as the pipeline runs it. Quality grids are
every 2 for JPEG and WebP and every 5 for AVIF; the crossing of each target
is interpolated between the two grid points either side.

The native tier is swept with `--features native`, which needs the same
tools as `sqzer --features native` (cmake, a C++ compiler, nasm, a system
`libheif`; see ADR-0004). A native backend takes its format over from the
portable one, so a native sweep measures `webpx`, `gamut-jxl`, `libavif`
and `jpegli` and a portable sweep measures `mozjpeg-rs` and `ravif`; both
tables are kept, keyed by tier. Lossy WebP exists only in the native tier,
so its table comes from a native sweep.

```sh
# the native backends; run the portable sweep too, since each run
# overwrites the whole table file
cargo run --release --features native -- sweep --jobs 6
```

## Reading a table

```
target  quality   q25   q75  unreachable
    70     69.0    62    74  0
```

At target 70, half the corpus reaches the score by JPEG quality 69, a
quarter of it by 62 and three quarters by 74. `unreachable` counts images
that do not reach the target even at the top of the grid; those are left out
of the median. A target nothing reaches is seeded at the top of the grid,
where the search finds the cap in one encode.

The search takes the median as its first trial and half the interquartile
range, clamped to between 2 and 20, as the step it takes when that trial
misses. See `Search::seed_step` in `sqzer-metrics`.
