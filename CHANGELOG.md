# Changelog

## Unreleased

### ⚠️ BREAKING CHANGES

* **codecs:** `sqzer_codecs::png::PngEncoder`, the plain `png` writer, is removed. `sqzer_codecs::oxipng::OxipngEncoder` writes PNG on every target; effort 0 is its fastest setting

### Features

* **wasm:** the browser build is `sqzer` on npm, built and staged from the release tag with provenance, live once approved (ADR-0011 D5): `npm install sqzer`. Its JavaScript API (ADR-0011 D3): `optimize(bytes, options)` for the whole pipeline in one call, `decode` and `SqzerImage.encode` to decode once and encode as often as a slider moves, `decodeAny` to hand what the package cannot read (SVG, HEIC where the browser has it) to the browser's canvas, `fromPixels` for pixels made elsewhere, and `codecs()` for what the build carries. Options are the command line's flags in camel case under the same rules, `onTrial` reports each trial of the target search, a result is the `--json` record in camel case with `bytes` added, and an error is a `SqzerError` whose `kind` is the `sqzer_core::Error` variant. The package is the portable tier minus `svg`, and its pixel limit defaults to 24 megapixels
* **sqzer:** one feature per portable codec (`png`, `jpeg`, `webp-lossless`, `avif`, `jxl-decode`, `heif`, `gif`, `tiff`, `bmp`, `tga`, `ico`, `qoi`, `pnm`, `exr`, `svg`), forwarded to `sqzer-codecs`, for a build that leaves part of the tier out. `portable` is still all of them and still the default
* **codecs:** PNG on `wasm32` is written by `oxipng`, byte for byte what a desktop build writes from the same pixels, in place of the plain `png` writer, whose files were about 14 % larger. `libdeflate` is built there in its `freestanding` mode, so a `wasm32` build of `sqzer` needs `clang` on `PATH` (ADR-0011 D1)
* **codecs:** AVIF decodes on `wasm32`. The decoder is `sqzer-rav1d` on every target, in place of the archived `re_rav1d`: upstream `rav1d` `main` with its safe Rust API and a stand-in for the `libc` names `wasm32` lacks (ADR-0011 D2). A `wasm32` build now reads AVIF input, takes a perceptual target for AVIF output and defaults to AVIF for photos, like every other build. `--list-codecs` and `--json` name the decoder `rav1d`
* **cli:** errors and warnings style flags and values the way `clap` does: `'--width'` quoted, in yellow where it names the problem and in green where it names the fix. `--color never`, `NO_COLOR` and a pipe keep the quotes without the colour. The `error` and `reason` fields of `--json` carry the same text with backticks around each flag

### Performance Improvements

* **workspace:** `wasm32-unknown-unknown` builds of the workspace turn `simd128` on through `.cargo/config.toml`. The target search runs about 1.7x faster there; a build of your own crate over `sqzer` sets the flag itself

### Bug Fixes

* **workspace:** `wasm32-unknown-unknown` builds of the workspace link with an 8 MiB stack through `.cargo/config.toml`. The linker's 1 MiB overflows in an unoptimised build of the decoders, which on `wasm32` is a trap with no message. A build of your own crate over `sqzer` sets `-C link-arg=-zstack-size` itself

## 0.2.0

### ⚠️ BREAKING CHANGES

* **core:** `Resize` is the full request of ADR-0009 (`size`, `fit`, `position`, `background`, `enlarge`, `filter`) instead of `max_width` and `max_height`. `Resize::inside(width, height)` builds the old bounds, and `Resize::fit` returns an `Option<Geometry>` instead of an `Option<(u32, u32)>`; the output size is `Geometry::output`
* **sqzer:** `Sqzer::transform` returns a `Ready`, and `Sqzer::encode`, `encode_with` and `pick_format` take one (ADR-0010). A caller that encoded a `Decoded` directly calls `Sqzer::prepare` then `Prepared::resize`, or `Sqzer::transform`, first

### Features

* **cli:** resize flags of ADR-0009: `--width` and `--height` with `--fit inside|cover|contain|fill|outside` in CSS `object-fit` terms, `outside` setting the shortest side with a square box (ADR-0010), `--position` for where cover crops and contain places the image, `--background` for the padding of contain (a translucent one needs a format with alpha, else exit 3), `--scale 50%`, `--enlarge` to allow scaling up, and `--filter` (`lanczos3`, `mitchell`, `catmull-rom`, `bilinear`, `box`, `nearest`). `--max-width` and `--max-height` behave as before
* **cli:** `--width 480,960,1600` writes one output per width and format from one decode, named `photo-480w.avif` and so on for `srcset`, each width searched on its own. Widths that come out the same size are refused before anything is encoded
* **cli:** a dry run shows what a resize crops or pads, `48x32 -> 16x16 crop 32x32 at 0,0`
* **cli:** the `rimage` hint translates `--resize` specs into the new flags, `--enlarge` included unless the command passed `--no-upscale`
* **core:** the resize geometry of ADR-0009 as pure functions: `Resize::fit` gives the crop of a cover fit, the resampled size and the canvas of a contain fit, for the dry run and the pipeline alike
* **sqzer:** a type per stage, ADR-0010: `Sqzer::prepare` returns a `Prepared` once per input, `Prepared::resize` a `Ready` once per size, so several sizes come from one decode and one colour conversion. `encode` pads a contain fit with the background its encoder calls for, transparent where it takes alpha and white where it does not, and refuses a translucent one on an encoder without alpha with `Error::Unsupported`

### Bug Fixes

* **cli:** `-f png,png`, and a `--template` without `{ext}` or `{format}` under several `-f` formats, are argument errors instead of writing one file over another

## 0.1.1

### Features

* **codecs:** HEIC input keeps its Exif and XMP items, the Exif orientation tag reset since the container's `irot` and `imir` are what rotate a HEIF

### Bug Fixes

* **codecs:** AVIF input applies the container's rotation and mirroring (`irot`, `imir`), keeps its ICC profile and its Exif and XMP items, and turns primaries other than sRGB from an `nclx` box or the AV1 sequence header (Display P3, BT.2020) into an ICC profile for the colour stage. Before, all of it was ignored: a rotated AVIF came out on its side and a P3 one with its colours shifted, even with `--keep-icc` or `--keep-metadata`
* **codecs:** PNG input applies its EXIF orientation unless `--no-auto-orient`, and reads an `eXIf` chunk placed after the image data

## 0.1.0

The first release. Everything below is new.

### Features

* **cli:** the `sqzer` binary, ADR-0003: one flat command, `-f` for one or several output formats, a perceptual target by default, `-t`, `-q`, `--lossless`, `--preset` (`web`, `thumbnail`, `archive`, `lossless`) and `--fast` to change it, `-e` for effort
* **cli:** codec-specific options through `-x codec:key=value` only, checked against the backend before any file is touched. `--list-codecs` and `--list-codecs -v` print every backend in the build, its options and defaults, and which metadata it can carry
* **cli:** output placement: a sibling file by default, `-o` file or directory, `--suffix`, `--template` with `{stem} {ext} {width} {height} {format} {quality} {dir} {name}`, `--in-place` with `--backup`, `-r` mirroring each input tree under `-o`. An output never overwrites its input unless `--in-place`, and one larger than its input is skipped unless `--force`
* **cli:** inputs: an argument that exists is a literal path, one that does not and has glob characters is expanded in-process and case-insensitively. `--files-from` with `-0`, `--include` / `--exclude` under `-r`, `-` for stdin to stdout
* **cli:** `--max-width` and `--max-height`: fit inside the bounds, keep the aspect ratio, never enlarge. The `thumbnail` preset fits inside 512 x 512
* **cli:** `--keep-icc` and `--keep-metadata`. By default an ICC profile is converted to sRGB and dropped and EXIF and XMP are stripped; EXIF orientation is applied either way
* **cli:** `-j` files in flight, bounded by an estimate of each file's peak memory, so a folder of large photos cannot exhaust the machine (ADR-0008). A file over the budget on its own runs alone with a warning
* **cli:** `--json` writes one JSON Lines object per output and nothing else to stdout; `-n` decodes and plans without encoding; `-v` shows search trials, `-vv` resolved codec options. Exit codes 0 (done), 1 (partial batch), 2 (arguments), 3 (nothing could be done)
* **cli:** coloured per-file lines, a summary line after a batch and a progress bar on a terminal, coloured by stage; `--quiet`, `--progress`, `--color` and `NO_COLOR`
* **cli:** a format with no encoder in this build is an error naming the feature or the release archive that has one, never a fallback to another format. A `rimage` codec name as the first argument prints the equivalent `sqzer` line
* **sqzer:** `Sqzer`, the library facade: `decode`, `transform` and `encode`, or `run` for the three in order, and `encode_with` to report progress to an observer. `Output` carries the bytes, the backend, its tier, the content class and the search report
* **sqzer:** the perceptual target is the default: encoder quality is searched until the output scores SSIMULACRA2 70, in at most six encodes, starting from a seed table calibrated per backend
* **sqzer:** a format picked by content when none is given: AVIF for photos, lossless WebP for graphics (PNG in a build without a WebP encoder)
* **sqzer:** the colour stage over `moxcms` (ADR-0007), the resize stage over `fast_image_resize` (Lanczos3 in linear light, premultiplied alpha) and the metadata stage. Float input is converted to 16-bit sRGB, clipped at display white, no tone mapping
* **codecs:** portable encoders, pure Rust apart from `oxipng`'s vendored `libdeflate` (ADR-0002): JPEG over `mozjpeg-rs`, PNG over `oxipng` (the plain `png` writer on `wasm32`), lossless WebP over `image-webp`, AVIF over `ravif`
* **codecs:** portable decoders: JPEG over `zune-jpeg`, PNG over `png`, WebP over `image-webp`, AVIF over `re_rav1d` (desktop only), JPEG XL over `jxl-oxide`, GIF over `gif`, TIFF over `tiff`, BMP, TGA, ICO, QOI and PNM over `image`'s decoders, OpenEXR over `exr` and SVG through `resvg`
* **codecs:** the native tier, ADR-0004, one feature per library: `native-webp` (lossy and lossless WebP over `libwebp`), `native-jxl` (JPEG XL over `libjxl`), `native-avif` (AVIF over `libavif` and `libaom`), `native-jpegli` (JPEG over jpegli) and `native-heif` (HEIC input through ImageIO on macOS, WIC on Windows and a runtime-loaded `libheif` on the other targets except musl, ADR-0005). A native encoder takes its format over from the portable one
* **codecs:** the JPEG, WebP, JPEG XL, TIFF and HEIC decoders apply the image's orientation; PNG keeps EXIF but does not apply its orientation. Decoders keep an embedded ICC profile, except GIF, whose rare ICC extension is not read, and JPEG XL, which renders XYB and enumerated colour encodings to sRGB instead. JPEG, PNG, WebP and JPEG XL also keep EXIF and XMP, TIFF keeps XMP. The AVIF decoder reads none of ICC, orientation, EXIF or XMP, since `avif-parse` does not expose them. Encoders embed what they can carry and say so in `EncoderCaps`
* **metrics:** SSIMULACRA2 over `fast-ssim2` and `Search`, the target-quality bisection: a hard cap of six encodes, and a target the encoder cannot reach returns the best candidate with `reached: false`, never an error
* **metrics:** seed tables for every lossy backend, portable and native, generated by `tools/calibrate` from CID22, GB82-SC and CLIC 2025. On a held-out split the search takes two to four encodes instead of four to six
* **core:** `Image` with `u8`, `u16` and `f32` samples and ICC, EXIF and XMP on the struct; the `Decoder`, `Encoder` and `Metric` traits described by `DecoderCaps` and `EncoderCaps`; `Registry`, `EncodeParams`, `Preset`, `Resize`, `Orientation`, and `Error` with `EncoderUnavailable` and `DecoderUnavailable`
* **workspace:** release binaries for six desktop targets with the native tier, built by `cargo-dist` (ADR-0006), plus shell and PowerShell installers and a Homebrew formula, `brew install sqzer-dev/tap/sqzer`. `native` on `sqzer` and `sqzer-cli` means every backend the target can build and run; a single `native-*` feature is strict
* **workspace:** golden SSIMULACRA2 tests for every encoder, so a dependency bump that degrades output fails CI; CI on the six desktop targets and `wasm32`; `cargo deny` with a permissive licence allow-list
