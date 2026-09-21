# ADR-0003: CLI interface

**Status:** Proposed
**Date:** 2026-09-07
**Deciders:** Vlad (sole maintainer)
**Scope:** The full grammar of the `sqzer` binary. ADR-0001 D5 fixed the six command shapes, the exit codes and `--list-codecs`; this record keeps all of that and fills in everything around it: flag names, output placement, path handling, parallelism, feedback channels, and the migration path for `rimage` users. Nothing here changes D4 (defaults policy) or D7 (metadata policy). Closes ADR-0001 action item 7 when accepted.

---

## Context

Two inputs shaped this record: the `rimage` issue tracker, and the documented interfaces of the tools people already have in their shell history.

### What `rimage` taught

`rimage` had two CLI generations. v0.x was flat with `-f <codec>` (#25). v0.11 moved to one subcommand per codec (`rimage mozjpeg -q 75 files...`, #175, PR #188), because `-q` silently did nothing for JPEG XL (#137). That fix made every codec's flags local to its subcommand, and the tracker shows what that cost. 74 issues, grouped:

```text
flag placement      #220 `png f.png -q 90` fails, same line works under `moz`; clap suggests `-- -q`
                    #206 `rimage webp *.png -f webp -r` -> "webp is not supported codec" (v0.x habit on v0.11)
                    #223 `<FILES>...` cannot be split around flags
                    #341 mozjpeg `--quantization` "doesn't work, use -q": two flags that both read as quality
                    #80 #123 #191 #190  "what changed?", "`-t` disappeared", "better help", "better README"

paths and globs     #360 Windows: `rimage oxipng *.png` hangs forever (v0.12.4 regression, PR #370)
                    #67 trailing `\`, #114 `-` `[` `]`, #91 brackets, #265 "extremely special filename",
                    #266 dots in the name break the output name, #386 trailing space, #93 `.JPG` not matched
                    #374 file list as input (shipped v0.13)

output placement    #50 output dir, #171 `-r`, #327 `--suffix` doc wrong, #339 no output on Windows since 0.11,
                    #342, #261, #254 recursive broken in a prerelease

colour / metadata   #104 rotated 90° and resized wrong (EXIF orientation not applied before resize)
                    #138 colours shift when an ICC tag is present, #221 "lips turn pale" at mozjpeg q90
                    #135 #211 keep EXIF, #316 EXIF reader panics on a file with no metadata

resources           #343 #340 default parallelism over huge images exhausts memory (PR #345)
                    #90 progress bar spams a non-TTY
                    #307 print dimensions so a script can decide whether to resize; #357 `--reduce-only`
                    #379 chained `--resize` all computed from the original, #378 garbage accepted, #224 aspect

build / availability #101 hard-coded Homebrew `libwebp` path, #210 `"webp": No such file`, #126 `libstdc++-6.dll`,
                    #321 #269 build failures, #144 `wasm-pack` (open since 2023), #395 replace `libavif` (open),
                    #133 jxl output is 0 KB
```

The last group is not a CLI problem, but the CLI is where the user finds out, and "webp is not supported codec" told them nothing.

### What the competitors agree on

`oxipng`, `cwebp`/`dwebp`, `cjxl`/`djxl`, `avifenc`, `cjpeg`/`jpegtran`/`jpegoptim`, `pngquant`, `caesiumclt`, `sharp-cli`, `magick`/`mogrify`, `squoosh-cli`, `imagemin-cli`, `optimizt`, `image_optim`, `ect`, `zopflipng`. The conventions converge:

```text
tool [OPTIONS] <INPUT>...     flat flags, no codec subcommands. every tool except sharp-cli
-o, --output <PATH>           file or directory (cwebp, avifenc, pngquant, caesium; oxipng --out / --dir)
-q, --quality <0-100>         cwebp, cjxl, sharp, caesium, magick, cjpeg; avifenc --qcolor
--lossless                    cwebp, sharp, caesium, optimizt; avifenc -l
-e, --effort  /  -s, --speed  cjxl -e 1-9, sharp --effort, oxipng -o 0-6 (higher = slower)
                              avifenc -s 0-10, pngquant -s 1-11, cwebp -m 0-6 (mixed directions)
-r, --recursive               oxipng, image_optim, ect -recurse; caesium -R
-j, --jobs  /  --threads      avifenc -j, oxipng -t, caesium --threads, jpegoptim -w
--strip [all|exif|icc|xmp]    pngquant, oxipng --strip safe|all, jpegoptim --strip-*, cwebp -metadata
-n / -d, --dry-run            jpegoptim -n, oxipng -d, caesium -d, zopflipng -d
-v, --verbose  /  --quiet     nearly all; -v repeatable in oxipng, cjxl, rg
--json                        oxipng -j, caesium --json (progress on stderr, JSON on stdout)
-                             stdin / stdout in oxipng, cwebp, cjxl, pngquant, magick
--force                       write even if larger / overwrite (oxipng, pngquant, jpegoptim)
skip if larger, by default    oxipng, jpegoptim, ect, zopflipng, image_optim
```

Nobody does `tool <codec> [codec flags] files`. `rimage` v0.11 stands alone with it. Codec-specific tuning is either namespaced long flags (`caesiumclt --jpeg-chroma-subsampling`), a generic passthrough (`avifenc -a end-usage=q`, `cjxl -x strip=exif`, `magick -define webp:lossless=true`), or dotted flags (`imagemin --plugin.webp.quality=95`).

### What the scaffold already fixes

`EncodeParams` has exactly five fields: `target`, `effort`, `subsampling`, `keep_icc`, `codec_specific` keyed `codec:key`. `DecodeOpts` has `max_pixels` and `apply_orientation`. `Error` has `EncoderUnavailable { format, available_in }`, `TooLarge`, `Unsupported`. `Sqzer` is a builder with `format`, `target`, `effort`, `subsampling`, `codec_opt`, `max_pixels`. And the rule in `CLAUDE.md`: adding a format must not require changes to the CLI. The grammar has to be a thin projection of those types, nothing more.

---

## Decision

One flat command. The six shapes from D5 stay as written:

```sh
sqzer photo.jpg                          # -> photo.avif next to it (content-aware default format)
sqzer photo.jpg -f webp,avif,jxl         # one input, three outputs
sqzer ./assets -r -f avif -o ./dist      # recurse, mirror the tree into dist
sqzer *.png --preset lossless -f webp    # lossless conversion
sqzer in.png --target 60 --max-width 1600
sqzer in.png --json                      # sizes, scores, chosen params, one object per input
sqzer --list-codecs                      # what this build can decode and encode, and from which tier
```

### Universal flags: one per `EncodeParams` field, nothing else

```text
-t, --target <SCORE>          Target::Ssimulacra2. default 70 (from the preset)
-q, --quality <0-100>         Target::Quality. disables the search. exclusive with --target
    --lossless                Target::Lossless. exclusive with both
-e, --effort <0-10>           higher is slower and smaller, always. backends map it (cwebp -m, avifenc -s inverted)
    --subsampling <auto|444|422|420>
    --keep-icc                keep the profile instead of converting to sRGB
-x, --codec-opt <codec:key=value>   repeatable. the only codec-specific path. unknown keys are errors
    --preset <web|thumbnail|archive|lossless>
    --fast                    skip the search: encode once at the seed-table quality for the target
```

`-q` is the flag every competitor uses for quality and the one `rimage` users reach for first (#137, #220, #341 all involve it). It means `Target::Quality` and nothing else. Quiet is `--quiet`, long only. `-t` was threads in `rimage` (#123); it is target here because target is the product, and the migration table says so.

Codec-specific knobs never become flags. `--codec-opt jpeg:progressive=false` is ugly on purpose: it is the escape hatch D4 named, it is what `EncodeParams::with_codec_opt` already takes, and it is the only way a new backend costs the CLI zero lines. If a key gets typed often, the answer is a preset, not a flag.

### Output placement

```sh
# default: sibling file, same stem, new extension. never the input path
sqzer photo.jpg                       # photo.avif
sqzer photo.jpg -f jpeg               # error: output would overwrite input. use -o, --suffix or --in-place

-o, --output <PATH>     file when one input and one format and PATH has an extension; directory otherwise
    --suffix <S>        photo{S}.avif
    --template <T>      "{stem}-{width}w.{ext}", also {dir} {name} {format} {quality}
    --in-place          write over the input. only meaningful when format is unchanged
    --backup            with --in-place: keep the original as photo@backup.jpg (rimage's flag, kept)
-r, --recursive         directories; the tree under each input root is mirrored under -o (#171)
    --force             overwrite an existing output; write even when larger
```

Skip-if-larger is on by default: an output larger than its input is not written, the JSON line says so, exit code is still 0 for that file. Every optimiser in the survey does this and nobody has filed a bug against it.

### Inputs and paths

The rule that ends #360, #114, #91, #265, #266, #386:

```text
1. an argument that exists on disk is a path. no glob parsing, ever
2. otherwise, if it contains a glob metacharacter, expand it in-process (Windows shells do not)
3. otherwise, error: no such file. if the argument is a rimage codec name, add the rewrite hint
```

Extension matching is case-insensitive (#93). The stem is everything before the last dot and the output name is built from the parsed stem, never by string replace (#266). A trailing `\` on Windows is a quoting artefact of `cmd.exe`; strip one trailing quote character before resolving (#67).

```text
-                       read one image from stdin, write to stdout (needs -f)
--files-from <FILE|->   one path per line (#374). -0 for NUL-separated, fd/rg -0 style
--include / --exclude <GLOB>   filter inside -r
```

Format is inferred from bytes by the registry's `probe`, never from the extension, so a `.jpg` that is a PNG decodes as a PNG (#143).

### Metadata and colour

Follows D7 unchanged, spelled as flags:

```text
default             strip everything, convert ICC to sRGB, apply EXIF orientation then drop it (#104, #138)
--keep-metadata     keep EXIF and XMP (#135, #211)
--keep-icc          EncodeParams::keep_icc
--no-auto-orient    DecodeOpts::apply_orientation = false
```

One flag pair, not a `--strip` grammar. The survey shows `--strip` with an allow-list in `oxipng`/`jpegoptim`/`cwebp`, but the default there is keep. Here the default is strip, so the flag says what to keep.

### Resize

```text
--max-width <N>, --max-height <N>   never enlarge (#307, #357). the D5 flags
--resize <WxH|Nw|Nh|Nl|Ns|N%>       explicit geometry, rimage's grammar, may enlarge
--filter <lanczos3|...>
```

One resize per invocation (#379). The spec parser is strict: `1600w` parses, `1600wx` errors (#378).

### Parallelism and memory

```text
-j, --jobs <N>          files in flight. default available_parallelism(). rayon in the CLI, per D3
--max-pixels <N>        DecodeOpts::max_pixels. default 268M
--threads <N>           thread budget handed to each encoder. default 1, per D3
```

`-j` is bounded by a decoded-pixel budget as well as a file count: the scheduler adds the header dimensions of the next file to a running total and waits when it would exceed `max_pixels * jobs / 4`. Two 200-megapixel inputs never decode at once on an 8-thread box (#343, #340). `--threads` defaults to 1 because rav1e and libaom fighting `rayon` for cores halves throughput (D3).

### Feedback channels

```text
stdout      --json: one object per input per output format, JSON Lines. nothing else, ever
stderr      progress (only when stderr is a TTY, or --progress always), warnings, errors
--progress <auto|always|never>
--quiet     no progress, no warnings. errors still print
-v          repeatable. -v shows the search trials, -vv shows codec options as resolved
--color <auto|always|never>, NO_COLOR honoured
-n, --dry-run   decode, probe, resolve every output path, print what would happen. with --json this
                is the answer to #307: width, height, alpha, format, and the planned outputs
```

`--dry-run --json` replaces the `info` command that `rimage` never had. It costs one decode and no encode.

### Exit codes

From D5, unchanged:

```text
0   every input produced every requested output (skipped-as-larger counts as success)
1   at least one input failed. failures are listed on stderr and in --json
2   argument error, including --target with --quality, unknown --codec-opt key, missing -f on stdin
3   nothing could be done: no input matched, or no encoder for the format in this build
```

`oxipng` returns 0 if any file succeeded. D5 returns 1 for a partial batch, which is what a CI step wants. Kept.

### Availability errors

`Error::EncoderUnavailable` already carries `available_in`. The CLI renders it:

```text
error: no WebP encoder in this build for lossy output
  this build encodes: jpeg (mozjpeg-rs, portable), png (oxipng, portable), avif (ravif, portable),
                      webp lossless (image-webp, portable)
  lossy webp needs the `native-webp` feature, or a native build from the releases page
  run `sqzer --list-codecs` for the full list
```

That one message is the CLI-side answer to #101, #126, #206, #210 and #395.

### `rimage` migration

The seven `rimage` codec names (`mozjpeg`, `oxipng`, `webp`, `avif`, `jxl`, `png`, `jpeg`) as a non-existent first positional print a rewrite and exit 2:

```text
error: `sqzer mozjpeg` is rimage syntax. try:
    sqzer -f jpeg -q 75 ./in.jpg
```

The README carries the mapping: `mozjpeg -q` → `-f jpeg -q`, `oxipng` → `-f png`, `-d` → `-o`, `-s` → `--suffix`, `--quantization` → `--codec-opt png:colors=`, `-t` → `-j`.

---

## Options considered

### A. Codec subcommands, the `rimage` v0.11 shape

`sqzer mozjpeg -q 75 --resize 50% ./in.jpg`. Familiar to existing `rimage` users, unknown to everyone else. Every codec is a flag namespace, so shared flags either repeat or hoist to global and users cannot tell which is which (#220). No competitor does it. Rejected.

### B. Flat flags, format by `-f` or by the `-o` extension. Chosen

The shape D5 already sketched and the shape of every tool in the survey except `sharp-cli`. One flag namespace; order never matters; `-f webp,avif,jxl` and `--template` fall out naturally. The cost is that `--help` gets long and the `-q` / `--effort` mapping per codec has to be documented and tested. Both are bounded.

### C. Verb subcommands, `sqzer convert` / `sqzer optimize` / `sqzer info`

Clean extension point, and `git`/`cargo` users like it. But `optimize` is `convert` with the format unchanged, users will not hold that distinction, and image tools never make you type a verb. `info` is covered by `--dry-run --json`, `codecs` by `--list-codecs` (D5). Rejected. If a real verb ever appears (`bench`, `calibrate`), it can be added next to positionals with clap's `args_conflicts_with_subcommands` without breaking anything.

### D. Namespaced per-codec flags, `--jpeg-chroma`, `--avif-depth`

What `caesiumclt` does. Discoverable in `--help`, and the first draft of this record proposed it. Rejected because it breaks the rule that a new backend costs the CLI nothing, and because `EncodeParams::codec_specific` is already the designed home for these knobs. `--codec-opt` is the same information with one flag instead of forty.

### E. JSON blobs per codec, `--webp '{"quality":80}'`

`squoosh-cli`, archived. Quoting JSON in `cmd.exe` and PowerShell is exactly the population that filed #67, #114 and #386. Rejected.

---

## Trade-offs

**Broad familiarity over `rimage` familiarity.** Option A is familiar to ~400 stars' worth of users; B is familiar to everyone who has typed `cwebp -q 80 in.png -o out.webp`. The second group is larger and is the group `sqzer` exists to win. The hint-on-codec-name and the migration table make the first group's cost a one-time one.

**`--codec-opt` over per-codec flags.** Discoverability suffers: nobody will guess `png:colors`. Mitigation is `--list-codecs -v`, which prints every backend's accepted keys with their defaults, generated from the backend, not maintained by hand. That is the same information a `--help` heading would carry, at zero CLI cost per format.

**`-q` means quality, not quiet.** `oxipng` uses `-q` for quiet; `cwebp`, `cjxl`, `sharp` use it for quality. The image-tool majority wins, and quiet is rare enough on an optimiser to be long-only.

**`--target` as `-t`.** `rimage` had `-t` for threads and it disappeared without a note (#123). Reusing the letter for something else is a small trap for one group of users, in exchange for the product's headline feature getting the short flag. The migration table names it.

**Strict skip-if-larger and refuse-to-overwrite defaults.** They produce more "nothing happened" moments than `rimage` had. Every such moment prints one line saying why and which flag changes it. That is cheaper than one clobbered original.

**Threads at 1 per encoder.** Slower on a single large image, faster on a folder, and the pixel budget makes the folder case safe. `--threads` exists for the single-image case.

---

## Consequences

- Flag order stops mattering, `-q` means one thing everywhere, and the whole grammar is a projection of `EncodeParams`, `DecodeOpts` and `Target`. A new backend registers itself and the CLI does not change.
- `--help` is long and has to be grouped: `-h` shows the universal flags and the six examples, `--help` shows everything, the `cjxl -h` / `-h -v` tiering.
- Every universal flag needs a per-backend mapping test (`--effort 10` reaches `oxipng` as zopfli, reaches `ravif` as speed 0). That is the price of #137 never returning.
- `--dry-run --json` has to be a first-class path in `sqzer-cli`, not a debug print, because scripts will depend on it (#307).
- The pixel-budget scheduler is new code in `sqzer-cli` with no library equivalent. It stays in the CLI; the library takes one image at a time.
- Revisit: the `{template}` placeholder set once responsive-image users say what they need. Ship `{stem} {ext} {width} {height} {format} {quality} {dir} {name}` and no more.
- Revisit: whether `--list-codecs -v` is enough discoverability for `--codec-opt`, after the first month of issues on the public repo.

---

## Action items

1. [x] `clap` derive in `sqzer-cli` with the grammar above; `-h` / `--help` tiering; `--color` and `NO_COLOR`. (`--max-width` and `--max-height` arrived with the resize stage. `--resize`, `--filter`, `--keep-metadata` and `--threads` are not in the grammar yet: the stage only scales down with Lanczos3, and the library has no EXIF/XMP passthrough and no encoder thread budget. They land with those pipeline pieces rather than as flags that do nothing.)
2. [x] Path resolver: exists-is-literal rule, in-process glob on non-existent args, case-insensitive extensions, stem after last dot, trailing-quote strip. Test corpus with `[`, `]`, `-`, trailing space, trailing `\`, multiple dots, `.JPG`, CJK. Runs on the Windows CI job.
3. [x] Output resolver: `-o` file vs directory, `--suffix`, `--template`, `--in-place`, `--backup`, `-r` mirroring. Property test: output path equals input path only under `--in-place`. (Exhaustive over the flag grid rather than randomised.)
4. [x] Universal-flag mapping test per backend: `--quality`, `--effort`, `--subsampling`, `--lossless` reach each encoder as the documented value or return `Unsupported`. (The CLI maps flags to `EncodeParams` one to one and that mapping is tested; each backend's own mapping is tested in `sqzer-codecs`. `--codec-opt` keys are checked against `EncoderCaps::options`, which the caps test proves truthful.)
5. [x] `--files-from`, `-0`, `-` for stdin/stdout.
6. [x] Pixel-budget scheduler around `rayon`; `--jobs`, `--max-pixels`. (`--threads` waits for the encoder thread budget, see item 1.)
7. [x] `--json` line schema, `--dry-run`, `--progress`, `--quiet`, `-v` levels; exit codes 0/1/2/3 with a batch test for each.
8. [x] `EncoderUnavailable` renderer, `--list-codecs` and `--list-codecs -v` with per-backend option keys.
9. [x] `rimage` codec-name hint and the migration section in the README.
10. [x] `docs/adr/README.md` index entry; CHANGELOG under Unreleased, scope `cli`.

---

## Sources

- [rimage issues](https://github.com/vlad-salone/rimage/issues): [#175](https://github.com/vlad-salone/rimage/issues/175), [#220](https://github.com/vlad-salone/rimage/issues/220), [#206](https://github.com/vlad-salone/rimage/issues/206), [#360](https://github.com/vlad-salone/rimage/issues/360), [#343](https://github.com/vlad-salone/rimage/issues/343), [#307](https://github.com/vlad-salone/rimage/issues/307), [#104](https://github.com/vlad-salone/rimage/issues/104), [#101](https://github.com/vlad-salone/rimage/issues/101), [#144](https://github.com/vlad-salone/rimage/issues/144), [#395](https://github.com/vlad-salone/rimage/issues/395)
- [oxipng manual](https://github.com/oxipng/oxipng/blob/master/MANUAL.txt)
- [cwebp man page](https://github.com/webmproject/libwebp/blob/main/man/cwebp.1)
- [cjxl man page](https://github.com/libjxl/libjxl/blob/main/doc/man/cjxl.txt)
- [avifenc man page](https://github.com/AOMediaCodec/libavif/blob/main/doc/avifenc.1.md)
- [pngquant man page](https://github.com/kornelski/pngquant/blob/main/pngquant.1)
- [jpegoptim](https://github.com/tjko/jpegoptim)
- [caesiumclt usage](https://github.com/Lymphatus/caesium-clt/blob/master/docs/USAGE.md)
- [sharp-cli](https://github.com/vseventer/sharp-cli)
- [ImageMagick command-line processing](https://imagemagick.org/command-line-processing/)
- [@squoosh/cli](https://www.npmjs.com/package/@squoosh/cli), [imagemin-cli](https://github.com/imagemin/imagemin-cli)
- [Command Line Interface Guidelines](https://clig.dev/), [no-color.org](https://no-color.org/)