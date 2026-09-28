# ADR-0009: The resize interface

**Status:** Accepted
**Date:** 2026-09-27
**Deciders:** Vlad (sole maintainer)
**Scope:** How a user asks for a size: the resize flags of the CLI, the `Resize` type in `sqzer-core`, and what the resize stage does for each fit. Replaces the "Resize" section of ADR-0003 (`--resize` with `rimage`'s grammar, `--filter`), none of which beyond `--max-width` and `--max-height` was built. Nothing here changes the stage order of ADR-0007 D2 (orient, colour, resize, encode), the resampler (`fast_image_resize`, Lanczos3 in linear light with premultiplied alpha), or the rule that the metric scores the resized image.

---

## 1. Context

`0.1.0` ships one resize: `--max-width` and `--max-height` fit the image inside a box, keep the aspect ratio and never enlarge. That covers "make this smaller for the web" and nothing else. ADR-0003 planned the rest as `rimage`'s grammar:

```text
--resize <WxH|Nw|Nh|Nl|Ns|N%>       explicit geometry, may enlarge
--filter <lanczos3|...>
```

Before building it, it is worth looking at how the tools people already know express a size. Three vocabularies dominate.

```text
# ImageMagick geometry, shared by libvips' vipsthumbnail. Compact, and the flags are shell metacharacters
magick in.jpg -resize '800x600>' out.jpg      # shrink to fit inside, never enlarge
magick in.jpg -resize '800x600^' out.jpg      # fill the box, may overflow one side
magick in.jpg -resize '800x600!' out.jpg      # exact, ignore the aspect ratio
magick in.jpg -resize 50% out.jpg
vipsthumbnail in.jpg --size '800x600>' --smartcrop attention

# sharp, and CSS object-fit before it: named options
sharp(input).resize({ width: 800, height: 600, fit: 'cover', position: 'top', withoutEnlargement: true })

# URL services (imgproxy, Cloudinary, imgix): a fit keyword plus width and height
/rs:fill:800:600:0/                            # imgproxy: fit, fill, fill-down, force, auto
?w=800&h=600&fit=crop                          # imgix
```

Squoosh's CLI took a fourth route, one JSON object per option, which mirrors its web UI and is awkward to type in any shell. `cwebp -resize <w> <h>` with `0` to keep the ratio is the minimal form. `rimage`'s grammar is its own: suffix letters for width, height, longest and shortest side, and its tracker carries the confusion that follows (#224 aspect ratio, #378 garbage accepted, #379 chained resizes).

What the survey says about the operations people ask for:

```text
fit inside a box, keep aspect        everyone. the web default
cover a box, crop the overflow       thumbnails, cards, avatars; sharp's default fit
contain in a box, pad the rest       product images on a fixed canvas
exact size, ignore aspect            rare, but every tool has it
scale by a factor                    ImageMagick 50%, rimage N%
never enlarge                        ImageMagick '>', sharp withoutEnlargement, imgproxy fill-down
several widths in one run            srcset. none of the CLIs above
where the crop lands                 gravity, position; libvips also crops on saliency
```

The one thing a web optimiser needs that no CLI above does in one call is the last but one: a responsive image is several widths of the same picture, and today that means one run per width, each decoding the source again.

## 2. Decision

### D1. Named flags in CSS `object-fit` terms

```text
--width <N>[,<N>...]      target width; a list gives one output per width (D3)
--height <N>              target height
--fit <inside|cover|contain|fill>
                          how the image meets the box, default inside (D2)
--position <center|top|bottom|left|right|top-left|top-right|bottom-left|bottom-right>
                          where cover crops and contain places the image, default center
--background <colour>     the padding of contain; by default transparent for an encoder
                          that takes alpha and white for one that does not, per output (D2)
--scale <N%>              scale by a factor instead of a box, keeps the aspect ratio
--enlarge                 allow scaling up; without it no fit ever scales up (D2)
--filter <lanczos3|mitchell|catmull-rom|bilinear|box|nearest>
                          default lanczos3; nearest for pixel art
--max-width <N>, --max-height <N>
                          kept, as the shorthand for --width / --height with --fit inside
```

`cover`, `contain` and `fill` mean what they mean in CSS `object-fit`, which every web developer has used and which `sharp` already maps to an image library. `inside` is `sharp`'s name for fitting inside the box without padding; with the never-enlarge default of D2 it behaves like CSS `scale-down`. It is the default because it is the web case.

Rules the parser enforces, each an argument error (exit 2):

```text
cover, contain and fill need both --width and --height
--scale excludes --width, --height, --max-width and --max-height
a --width list takes one --height at most, applied to every width
a --width list with a --template that lacks {width}, or with -o naming a file
--position without cover or contain, --background without contain
```

There is no `--resize` flag. `rimage`'s specs map onto these flags in the README migration table and in the hint a `rimage` command line prints (ADR-0003):

```text
rimage --resize 1600w      sqzer --width 1600
rimage --resize 50%        sqzer --scale 50%
```

The remaining specs are mapped against `rimage`'s own parser when the table is written (action item 4), since whether its `WxH` stretches or fits decides between `--fit fill` and `--fit inside`.

### D2. What each fit does

```text
inside     scale to fit inside the box, keep the aspect ratio. one side may be given alone
cover      scale to cover the box, keep the aspect ratio, crop the overflow at --position
contain    scale to fit inside the box, keep the aspect ratio, pad to the box with --background
fill       scale each axis to the box, the aspect ratio changes
```

Without `--enlarge` the scale factor is capped at one on each axis. For `inside` that means an image already inside the box is left alone, as `--max-width` does today. For `cover` the crop still happens, at the source's own resolution, so the output keeps the box's aspect ratio but may be smaller than the box (imgproxy's `fill-down`). `contain` pads a small image to the full box, since padding adds no invented pixels. `fill` caps each axis on its own.

The crop of `cover` is `fast_image_resize`'s `SrcCropping::FitIntoDestination`, with `--position` as its centring point, so the crop and the resample are one pass. The padding of `contain` is a copy into a canvas of the box size, and it happens in `Sqzer::encode` once the encoder is known, not in the shared resample (D4). One resampled image serves every format of that size, and each output pads it with its own background: `--background` when given, otherwise transparent where the encoder takes alpha and white where it does not. `-f jpeg,webp` with `--fit contain` writes a white-padded JPEG and a transparent-padded WebP. The copy is cheap next to an encode, and the metric scores the padded image the encoder receives. Neither the crop nor the pad is a new resampler, and neither needs a new dependency.

### D3. Several widths, one decode

A list of widths produces one output per width per format, from one decode, the way `-f` already produces one per format:

```sh
# six files: 480, 960 and 1600 wide, each as AVIF and WebP
sqzer photo.jpg --width 480,960,1600 -f avif,webp
```

Each width gets its own resize from the colour-managed source (never from the previous width, #379) and its own target search, since a smaller image needs different settings for the same score. With more than one width the names must differ per width. Without `--template`, the name gains `-{width}w`, the `srcset` descriptor, after the stem and any `--suffix`: `photo-480w.avif`, or `photo-min-480w.avif` with `--suffix -min`. A `--template` must contain `{width}`, and `-o` naming a file takes one width only; both are argument errors otherwise (D1). The planner also rejects two planned outputs with the same path, whatever produced them, before anything is encoded. `--json` already reports `output_width` and `output_height` for each output.

### D4. The library type

`Resize` in `sqzer-core` grows from two optional bounds into the full request:

```rust
pub struct Resize {
    pub size: Size,          // Box { width: Option<u32>, height: Option<u32> } or Scale(f32)
    pub fit: Fit,            // Inside, Cover, Contain, Fill
    pub position: Position,  // nine points
    pub background: Option<[u8; 4]>,
    pub enlarge: bool,
    pub filter: Filter,      // mapped to fast_image_resize's filter types in the facade
}
```

`background: None` means the per-output default of D2. `Resize::fit` stays the pure geometry function and gains the crop box and the padding for the other fits, so the CLI's dry run and `{width}` in templates keep asking the same function.

`Sqzer::transform` today takes the decoded image by value and does colour and resize in one call, so it cannot serve several widths from one source. It splits along the stage order of ADR-0007 D2:

```rust
// once per input: colour to sRGB, float to 16-bit, the metadata policy
pub fn prepare(&self, decoded: Decoded) -> Result<Decoded>;
// once per width: borrows the prepared image, returns a new one; cover crops here,
// contain records the canvas it still owes on the returned image
pub fn resize(&self, prepared: &Decoded, resize: &Resize) -> Result<Decoded>;
```

`Decoded` gains a pending canvas: the box size, the position and the `--background` choice, set by `resize` for `contain` and empty otherwise. `Sqzer::encode` applies it after picking the encoder and before the search, with that encoder's alpha deciding the default background. The padding therefore happens on every path, `run`, `transform` then `encode`, or the CLI's own calls, and no caller can encode a `contain` image without its canvas.

Colour conversion runs once, and every width starts from the same colour-managed source (#379). `transform` stays as `prepare` then `resize` with the builder's `Resize`, so a caller with one size does not change. A list of widths is the CLI's business: it calls `prepare` once, `resize` per width and `encode` per output, and the padding comes with `encode`. The prepared image stays alive while the widths run, which the ADR-0008 estimate has to count (action item 3). `Preset::resize` keeps the `thumbnail` box as `inside` 512 x 512.

## 3. Options considered

**`rimage`'s grammar, as ADR-0003 wrote it.** Familiar to people coming from `rimage`, compact. Rejected: suffix letters have to be learned and cannot express a crop, a pad or a position, and the tracker shows the grammar confusing its own users. The migration table and the `rimage` hint carry its users over at no cost.

**ImageMagick geometry.** The most widely known compact form, and expressive: `>` `<` `^` `!` `%` `@` cover every case in section 1. Rejected because those characters are shell syntax. An unquoted `--resize 800x600>` redirects the output of the whole command into a file named after the next word, and `^` is the escape character of `cmd.exe`. A flag that is correct only when quoted is a trap in a tool that also targets Windows. Its cryptic modifiers are also the opposite of `-h` being readable.

**One JSON option, as the Squoosh CLI.** Mirrors a UI and a library object one to one. Rejected: quoting JSON differs per shell, and error messages point into a string instead of at a flag.

**Positional fields, as imgproxy.** Compact in a URL, where it lives. Rejected on a command line for the same readability reason: `fill:800:600:0` says nothing without the documentation open.

**Smart crop on saliency, as vipsthumbnail's `attention`.** Useful for thumbnails of photos with an off-centre subject. Left out of this record: it needs a saliency or entropy analysis that `fast_image_resize` does not have, and no permissive crate for it has been surveyed. `--position` leaves room for an `attention` value later.

## 4. Trade-offs

- More flags than one `--resize <spec>`. They show up in `sqzer --help`, not in `sqzer -h`, which keeps `--max-width` and `--max-height` as the common case.
- `cover` without `--enlarge` can return an image smaller than the box. That is the honest result of "never invent pixels", and the dry run shows the size before anything is encoded.
- A width list multiplies the encodes: three widths and two formats are six target searches. That is the work being asked for. Whether the per-file estimate of ADR-0008 needs a term for several widths is checked in action item 3.

## 5. Consequences

- The "Resize" section of ADR-0003 is superseded by this record, and ADR-0003 carries a line at the top saying so.
- The README's `rimage` table and the `rimage` hint change from `--resize` to the flags of D1.
- The `thumbnail` preset and `--max-width` / `--max-height` behave exactly as in `0.1.0`.
- JSON and templates need no new fields; `{width}` and `{height}` are already the output's.

## 6. Action items

1. [ ] `Resize`, `Fit`, `Position`, `Filter` and `Size` in `sqzer-core` with the geometry of D2 as pure functions, tested on the corner cases: one side given, a box larger than the image with and without `--enlarge`, extreme aspect ratios, a one-pixel result.
2. [ ] The facade: `prepare` and `resize` of D4 with `transform` kept on top, the pending canvas on `Decoded` applied in `encode`; `SrcCropping::FitIntoDestination` for `cover`, the pad for `contain` in every layout and sample width with the per-encoder background, a library test that `run` with `contain` returns the full box; the filter mapping.
3. [ ] The CLI flags and rules of D1, the width list and naming of D3, the dry run showing crop and padding; check the ADR-0008 estimate for a file with several widths.
4. [ ] README: the resize examples and the `rimage` migration table; the `rimage` hint.
5. [x] On acceptance, the superseded line on ADR-0003.

## Sources

- [`sharp` resize options](https://sharp.pixelplumbing.com/api-resize)
- [CSS `object-fit` on MDN](https://developer.mozilla.org/en-US/docs/Web/CSS/object-fit)
- [ImageMagick image geometry](https://imagemagick.org/script/command-line-processing.php#geometry)
- [libvips `vipsthumbnail`](https://www.libvips.org/API/current/using-vipsthumbnail.html)
- [imgproxy resizing types](https://docs.imgproxy.net/usage/processing#resizing-type)
- [Squoosh CLI](https://github.com/GoogleChromeLabs/squoosh/tree/dev/cli)
- [`cwebp` options](https://developers.google.com/speed/webp/docs/cwebp)
- [`fast_image_resize` 6.1 `SrcCropping`](https://docs.rs/fast_image_resize/6.1.0/fast_image_resize/enum.SrcCropping.html)
- `rimage` issues #224, #307, #357, #378, #379, as surveyed in ADR-0003
