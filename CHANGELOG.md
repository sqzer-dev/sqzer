# Changelog

## Unreleased

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
