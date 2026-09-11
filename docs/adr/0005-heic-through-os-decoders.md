# ADR-0005: HEIC through OS decoders

**Status:** Proposed
**Date:** 2026-09-11
**Deciders:** Vlad (sole maintainer)
**Scope:** How a released `sqzer` binary reads HEIC without dying at startup on a machine that has no `libheif`. Covers the macOS and Windows system decoders, runtime loading of `libheif`, the feature shape, conformance between the decoders and what CI can prove. Answers the question ADR-0001 action item 9 (the `cargo-dist` release matrix) is blocked on, and supersedes ADR-0004 action item 4 (`native-heif` on Windows through vcpkg). Nothing here changes the tier rules of ADR-0001 D2 or the other four native backends of ADR-0004.

---

## 1. Context

ADR-0004 put HEIC input behind `native-heif`, which is `libheif-rs` over `libheif-sys` linking the system `libheif` dynamically through `pkg-config`, or vcpkg on Windows. That is the right shape for a source build on a machine that has the library. It is the wrong shape for a downloaded binary:

- A binary built with `native-heif` carries a `NEEDED libheif.so.1` entry on Linux and a load command for `libheif.1.dylib` on macOS. The dynamic loader resolves both before `main` runs, so `sqzer --version` refuses to start on a machine without the library, whether or not a HEIC is ever opened. `ld.so` prints `error while loading shared libraries: libheif.so.1`, `dyld` prints `Library not loaded`. The `czkawka` project hit exactly this with `libheif.so.1` in its release binaries.
- On Windows the vcpkg path is not even dynamic. `libheif-sys` 5.3.1 declares the triplet `x64-windows-static-md` and the port `libheif[aom]` for `x86_64-pc-windows-msvc`: a static `libheif` with an AV1 decoder and no HEVC decoder. A Windows build through that path recognises HEIC and fails at decode on every file. ADR-0004 action item 4 asked for this in CI; it is not worth having.
- `cargo-dist` has one `features` list for the whole matrix. There is no per-target feature override, so whatever `native` means has to build on all six desktop targets without a system library. Its `[dist.dependencies.homebrew]` entries with `stage = ["run"]` become `depends_on` in the generated Homebrew formula, which would keep the Homebrew install honest, but the shell installer, the MSI and a plain download from the releases page get nothing.

Two other facts shape the answer. Both platforms where most HEIC files live already ship a decoder: ImageIO on macOS since 10.13, and the Windows Imaging Component (WIC) HEIF codec on Windows 10 1809 and later when the Store extensions are present. And `libheif`'s C API is small and versioned, so loading it at runtime instead of link time is a bounded amount of work.

The eight questions this record answers, in the order they were asked:

```
1  does the full native release binary ship native-heif at all
2  does ImageIO on macOS cover the libheif backend's contract
3  how is the missing HEVC extension detected and reported on Windows
4  which binding crates, and what do they cost
5  conformance between three decoders, and the CI matrix
6  feature shape
7  Linux stays on a system library either way
8  runtime loading of libheif as an alternative
```

---

## 2. Decision

**Release binaries never link `libheif`.** HEIC input in a release build comes from the OS decoder where one exists and from `libheif` loaded at runtime everywhere else. `native-heif` keeps its name and keeps meaning "this build reads HEIC", but what it registers depends on the target:

```
macOS      ImageIO (`imageio`), then libheif loaded at runtime (`libheif`)
Windows    WIC (`wic`), then libheif loaded at runtime
Linux gnu  libheif loaded at runtime
Linux musl nothing; a static musl binary cannot dlopen (see D8)
wasm32     compile error, as for every native feature
```

The names in parentheses are what `DecoderCaps::name` reports and what `--list-codecs` prints, so a build says which decoder it has and whether that decoder is usable on this machine right now. The link-time `libheif-rs` backend is replaced by the loader; ADR-0004's pkg-config and vcpkg build requirements go away with it.

The rest of this section takes the eight questions in turn. Facts verified during this record are stated as facts; things that need a macOS or Windows machine to confirm are listed under "to verify" and again in the action items.

### D1. The release binary (question 1)

Four options:

```
a  drop native-heif from release builds     honest, no HEIC for anyone who downloads
b  two native flavours, with and without    doubles the matrix for one input format;
   libheif                                  cargo-dist cannot vary features per target
                                            anyway, so it is two dist configs
c  load libheif at runtime                  binary always starts; HEIC works wherever
                                            the library is installed; Windows has no
                                            standard place to get a heif.dll
d  OS decoders on macOS and Windows         zero install on the two platforms with the
                                            most HEIC users; nothing for Linux
```

Chosen: **c and d together**, d registered first. A missing library or a missing Windows extension becomes "no HEIC decoder on this machine", reported by `--list-codecs` and by the error for a HEIC input, never a binary that will not start. Option b is rejected outright: even if `cargo-dist` could express it, shipping a `sqzer-with-libheif` that refuses to launch is the failure this record exists to remove. Option a is what a release would have had to do this week, and stays the fallback if items 2 to 4 in section 6 slip.

Two partial alternatives were looked at and rejected. MSVC's `/DELAYLOAD` (plus `delayimp.lib`, as `rustup` does for optional DLLs) lets a Windows binary start without `heif.dll` and fail on first call, and macOS has weak linking; Linux has no equivalent, and the Windows vcpkg build has no HEVC decoder to delay-load in the first place.

### D2. ImageIO on macOS (question 2)

The backend is `CGImageSourceCreateWithData` + `CGImageSourceCreateImageAtIndex`, then the pixels through the image's data provider, or through a 16-bit `CGBitmapContext` when the decoder's own buffer is not a layout `Image` accepts.

Against the five HEIC fixtures and the libheif backend's contract:

```
orientation   ImageIO does not apply `irot` / `imir` in CreateImageAtIndex; it
              reports them as kCGImagePropertyOrientation. The thumbnail API
              with kCGImageSourceCreateThumbnailWithTransform applies them but
              hands back premultiplied BGRA. Decision: sqzer reads `irot` and
              `imir` from the container itself (a small ISOBMFF walk, see D6)
              and applies them through `Image::apply_orientation`, for every
              HEIC backend. Same answer for WIC below. `clap` is applied by
              ImageIO: every iPhone photo is a 512-pixel tile grid cropped to
              4032 x 3024 by `clap`, and Preview shows 4032 x 3024.
alpha         the alpha auxiliary image comes back folded into the CGImage as
              an alpha channel. Whether it is straight or premultiplied depends
              on the path: the data provider returns what the decoder produced
              (to verify), a bitmap context is premultiplied always, because
              Quartz supports no non-premultiplied RGB context format. Either
              way the backend unpremultiplies, with the helper the libheif
              backend already has.
ICC           CGImageGetColorSpace then CGColorSpaceCopyICCData (10.12+). For a
              `prof` box this should be the embedded bytes; for an `nclx` box
              ImageIO synthesises a profile, which is more than libheif does
              (libheif ignores `nclx`, ADR-0004). To verify: that the Display
              P3 bytes in `pattern-icc.heic` round-trip unchanged.
monochrome    to verify: whether the CGImage's colour space model is
              monochrome and one byte per pixel, or whether ImageIO expands
              to RGB. Both are acceptable; the test compares against the
              gray pattern either way.
10-bit        CGImageSourceCreateImageAtIndex returns a CGImage with 10 bits
              per component and 40 bits per pixel for a 10-bit HEIF (reported
              on the Apple forums, no documented layout). The backend does not
              unpack that; it draws into a 16-bit-per-component bitmap
              context (kCGBitmapByteOrder16Little with AlphaNoneSkipLast or
              AlphaPremultipliedLast, both in Quartz's supported list) using
              the image's own colour space so nothing is converted, and
              widens nothing: the context gives 16-bit samples. Needs a
              `pattern-10bit.heic` fixture, which does not exist yet.
```

Minimum OS: HEIF and HEVC support is built into macOS 10.13. Rust's `x86_64-apple-darwin` floor is 10.12 and `aarch64-apple-darwin` is 11.0, so on Intel the backend checks at runtime that `CGImageSourceCopyTypeIdentifiers` contains `public.heic`, and reports HEIC as unavailable on 10.12 instead of failing on the first file. That call is the macOS availability probe for `--list-codecs` too: it costs microseconds and, per Apple, every Mac on 10.13 or later can decode HEVC in software.

Hardware: Macs from 2016 on (Skylake and later for 8-bit, Kaby Lake and later for 10-bit, every Apple silicon Mac) decode HEVC in hardware; older Intel Macs fall back to software without any signal from ImageIO. `VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC)` would say which path is in use, and the answer changes nothing about the pixels, so the backend does not ask. GitHub's `macos-15-intel` runners have no HEVC block and exercise the software path, which is the one worth testing.

### D3. WIC on Windows (question 3)

Two Store packages are involved, and both have to be present:

```
HEIF Image Extension     Microsoft.HEIFImageExtension. Provides
                         CLSID_WICHeifDecoder, the container parser. Windows
                         10 1809+. Preinstalled on Windows 11 consumer
                         editions since 22H2. Not on Windows Server, not on N
                         editions, and absent on enterprise images where the
                         Store is blocked; Microsoft's answer there is Intune
                         deployment, there is no offline MSIX.
HEVC Video Extensions    Microsoft.HEVCVideoExtension ("from Device
                         Manufacturer", preinstalled by OEMs on many consumer
                         laptops) or Microsoft.HEVCVideoExtensions (the paid
                         Store listing). Provides the HEVC decoder MFT the
                         HEIF codec calls for `.heic`. Never preinstalled by
                         Windows itself, 24H2 included.
```

How the absence shows up. Without the HEIF package, `IWICImagingFactory::CreateDecoderFromStream` fails with `WINCODEC_ERR_COMPONENTNOTFOUND` (`0x88982F50`), which is what paint.net users see. With the HEIF package and without HEVC, the container decoder instantiates and the failure moves to the first frame decode, which is the premise of the question; the HRESULT at that point is not documented anywhere found for this record and has to be read off a machine. Microsoft's documented check is `MFTEnumEx` for a decoder of `MFVideoFormat_HEVC`. Two more constraints from people who ship on this path: the HEVC extension refuses images smaller than 8 x 8 with `E_INVALIDARG`, and the WIC HEIF decoder does not apply `irot` (ImageGlass 9 shipped every iPhone photo in landscape; `imageio-native` inserts an `IWICBitmapFlipRotator` from the orientation metadata by hand).

Decision on the probe: `--list-codecs` and the first HEIC input trigger one real decode of the embedded `pattern-rgb.heic` (769 bytes, 48 x 32, HEVC Main 4:2:0, above the 8 x 8 floor) through the same code path as a user file, cached in a `OnceLock` for the life of the process. It is the only check that answers the question the listing asks, "can this box decode a `.heic`", because enumerating WIC components or MFTs proves the presence of a package, not that the pair of them works. The cost is one `CoCreateInstance`, one decoder instantiation and the HEVC MFT's first load, expected in the tens of milliseconds and paid once; measuring it is an action item. `pattern-gray.heic` is smaller but monochrome HEVC is a range-extension profile the Windows decoder may not accept, so the RGB file is the probe.

Pixel formats: the backend asks WIC's format converter for `32bppRGBA` (straight alpha; `32bppPRGBA` is the premultiplied variant, so no unpremultiply step of our own) and `8bppGray` for monochrome. Whether the HEIF codec offers anything above 8 bits for a 10-bit source (`64bppRGBA`, `32bppRGBA1010102`) is unknown; if it does not, the backend returns 8-bit samples and says so in its docs, which is a documented deviation from libheif, not an error (see D6).

### D4. Binding crates (question 4)

```
crate                   version  licence                    MSRV  crates pulled  cold build here
objc2-image-io          0.3.2    Zlib OR Apache-2.0 OR MIT  1.71  3 (+bitflags)  1.0 s
  objc2-core-graphics   0.3.2    same                       1.71
  objc2-core-foundation 0.3.2    same                       1.71
windows                 0.62.2   MIT OR Apache-2.0          1.82  12 (+syn)      6.7 s
libloading              0.9.0    ISC                        1.88  1              n/a
```

Build times are `cargo build --release` of a probe crate with only the needed features (`CGImageSource`, `CGImage`, `CGDataProvider`, `CGColorSpace`, `CGBitmapContext`; `Win32_Graphics_Imaging`, `Win32_System_Com`, `Win32_Foundation`), cross-compiled from this Linux box with 12 cores. The `windows` rlib alone is 10 MB; the three `objc2` rlibs are 4 MB together. Both are a rounding error next to the four minutes the vendored C libraries take (ADR-0004 section 4). `syn`, `quote` and `proc-macro2` are already in the tree through `clap` and `serde`.

Licences are all on the `deny.toml` allow-list as it stands. MSRVs are all at or below the workspace's 1.92. One trap: `windows-core` 0.100 (September 2026) asks for Rust 1.95, and `windows` 0.62.2 pins `windows-core ^0.62.2`, so the pin holds today and a bump of `windows` past 0.62 has to be checked against the MSRV first. The `windows` crate is still the current binding for WIC; Microsoft's guidance to prefer focused crates over the umbrella applies where a focused crate exists, and there is none for WIC.

`unsafe` does not stay inside the bindings. Every `CGImageSource*` function in `objc2-image-io`, every COM method in `windows`, and `Library::new` / `get` in `libloading` are `unsafe fn`. `sqzer-codecs` is under the workspace `forbid(unsafe_code)` and cannot call any of them. So the rule from `CLAUDE.md` applies, "if a backend needs it, the backend crate has it": three small crates in the workspace, each with its own `[lints]` table (`unsafe_code = "allow"`, `unsafe_op_in_unsafe_fn = "deny"`, `missing_safety_doc` on), each exposing a safe API of two calls, an availability check and `decode(bytes, max_pixels) -> Raw` where `Raw` is planes plus geometry, alpha kind, bit depth, colour model and ICC bytes:

```
crates/heif-imageio   objc2-image-io, objc2-core-graphics, objc2-core-foundation
crates/heif-wic       windows
crates/heif-dl        libloading, the libheif FFI declarations of D8
```

`sqzer-codecs` depends on them as target-specific dependencies behind `native-heif` and keeps `forbid`. They are published with the workspace because `sqzer-codecs` on crates.io needs them; they are not public API and say so.

### D5. Conformance and CI (question 5)

Where the three decoders are known or suspected to differ:

```
grids          all three assemble grids; the iPhone default is a grid
clap           all three apply it (D2); libheif's double application of
               `clap` (GHSA-jc8f-p23p-5hjg, fixed in 1.23.1) was in the
               tiling API this backend does not use
orientation    libheif applies irot/imir; ImageIO and WIC do not. Resolved by
               sqzer applying them itself for every backend (D2, D6), so a
               backend's own behaviour no longer matters
alpha          libheif hands back stored samples and the `prem` flag; the
               ImageIO context path is premultiplied always; WIC converts to
               straight on request. Every backend returns straight alpha
bit depth      libheif: 10 and 12-bit planes; ImageIO: 10 bpc CGImage, taken
               through a 16-bit context; WIC: unknown, possibly 8-bit only
monochrome     libheif: a luma plane; ImageIO and WIC: to verify
ICC / nclx     libheif: `prof` bytes, `nclx` ignored; ImageIO: a profile for
               either; WIC: to verify
```

Decision: **the fixtures must decode identically on all three, within the tolerances the HEIC tests already use** (mean absolute error 6 for RGB and ICC, 4 for gray, 8 for the rotated file), with one class of deviation allowed and one forbidden. Allowed: a backend that cannot produce more than 8 bits from a 10-bit source returns 8-bit samples; the pixels are right and the test compares against the 8-bit pattern anyway. Forbidden: wrong pixels of any kind. A backend that cannot decode a fixture at all (WIC and monochrome, say) returns `Error::Unsupported` naming the backend and the reason, and the test for that fixture asserts the error on that backend; it never passes on a wrong image and never skips silently.

The fixture set grows by one, `pattern-10bit.heic`, made the same way as the other five (libheif with x265 at 10-bit, the 8-bit values widened by replication as `pattern-10bit.avif` is). A grid fixture would be worth having and libheif can write one; it is optional and not in the action items.

CI matrix after this record:

```
x86_64 / aarch64 linux-gnu   loader against apt libheif (`libheif1` and
                             `libheif-plugin-libde265`, the decoder is a
                             separate package since bookworm); all fixtures
x86_64 / aarch64 darwin      ImageIO, all fixtures; loader against brew
                             libheif, all fixtures; ImageIO registered first,
                             a second test registers the loader alone
x86_64 windows-msvc          WIC: `native` now builds in full because nothing
                             needs vcpkg. Whether the runner can decode is
                             an open question: GitHub's windows-latest image
                             is Windows Server, which has neither Store
                             extension by default. If they are absent the
                             job proves only the "unavailable" path: the
                             probe fails cleanly, --list-codecs says so, a
                             HEIC input gets the right error. The decode
                             tests then need a machine with the extensions,
                             run by hand before a release, until a runner
                             that has them is found
x86_64 linux-musl            loader compiled out; HEIC absent, as documented
```

### D6. Feature shape (question 6)

One `native-heif`. The alternative, `native-heif-imageio` / `native-heif-wic` next to the current feature, is more honest at build time but the target already decides which decoder a build can contain: `objc2-image-io` does not build on Windows, `windows` does not build on macOS, and Cargo's target-specific dependencies express that exactly. Three features would let a user ask for a decoder their target cannot have and get a compile error for it. Honesty moves to run time instead, which is where the interesting facts live anyway: `--list-codecs` names the backend and whether it is usable, and the error for a HEIC input names what is missing on this machine.

What changes to make that work, in `sqzer-core` and the CLI, and none of it is HEIC-specific:

- `Format::decoder_features`, the mirror of `encoder_features`, so `--list-codecs` prints `none; needs native-heif` instead of `none` for an input-only format.
- The HEIC brand sniff moves out of the libheif backend into the portable tier, together with a small ISOBMFF walk (`ftyp`, `meta`, `pitm`, `iprp`/`ipco`/`ipma`, `ispe`, `irot`, `imir`) that gives every build the format, the displayed dimensions and the orientation of a HEIC without any decoder. `avif-parse` already walks these boxes for AVIF and may accept HEIC brands with a relaxed check; if not, the walk is about 150 lines of safe Rust. A portable build then answers a HEIC with `Error::DecoderUnavailable { format: Heic, available_in: ["native-heif"] }` rather than `UnknownFormat`, and every HEIC backend applies orientation from the same source.
- `Decoder::available(&self) -> Result<(), String>` with a default of `Ok(())`. The OS backends and the loader answer from their cached probe. `--list-codecs` prints the reason next to the backend, the registry's probe skips a decoder that is unavailable so the next one registered gets the bytes, and a HEIC with no usable decoder returns `DecoderUnavailable` carrying the reason: "libheif.so.1 not found", "libheif 1.23.4 has no HEVC decoder plugin", "HEVC Video Extensions not installed".

`EncoderUnavailable` and `encoder_features` are untouched; HEIC has no encoder and never will (ADR-0001 section 1.1).

### D7. Linux (question 7)

There is no OS-level HEIC decoder on Linux to add. Everything that looks like one is `libheif` or `ffmpeg` underneath: the GdkPixbuf loader (`heif-gdk-pixbuf`) is built from the libheif tree, KDE's `kimageformats` and the Qt plugin link libheif, and GStreamer has no still-image HEIF element. `ffmpeg` 7.0 and later demux HEIF grids in `libavformat` and decode the tiles with `libavcodec`, which is a second runtime-loadable path in principle, but it needs two libraries bound at runtime, grid assembly and `clap` by hand, and a `libavcodec` whose ABI changes every major; it is libheif with more work. So the Linux answer is D1 and D8: load `libheif` at runtime, and say clearly when it is not there or cannot decode HEVC. Two distribution facts go into that message: Fedora's stock `libheif` has no HEVC decoder (patents; `libheif-freeworld` from RPM Fusion adds it), and Debian and Ubuntu ship the decoder as `libheif-plugin-libde265`, separate from `libheif1`.

### D8. Runtime loading of libheif (question 8)

`libheif-sys` cannot do it: its `build.rs` either probes `pkg-config` and emits a link line, or builds the vendored tree, or runs vcpkg. Nothing in it or in `libheif-rs` is written against function pointers. So the loader is a crate of our own, `heif-dl`, over `libloading` 0.9.

What it has to bind. The current `heif.rs` reaches, through `libheif-rs`, this set of C functions and nothing else:

```
heif_init  heif_deinit  heif_get_version_number
heif_context_alloc  heif_context_free
heif_context_read_from_memory_without_copy
heif_context_set_max_decoding_threads
heif_context_get_primary_image_handle  heif_image_handle_release
heif_image_handle_get_width  heif_image_handle_get_height
heif_image_handle_get_luma_bits_per_pixel
heif_image_handle_has_alpha_channel  heif_image_handle_is_premultiplied_alpha
heif_image_handle_get_preferred_decoding_colorspace
heif_image_handle_get_color_profile_type
heif_image_handle_get_raw_color_profile_size
heif_image_handle_get_raw_color_profile
heif_decoding_options_alloc  heif_decoding_options_free
heif_decode_image  heif_image_release
heif_image_has_channel  heif_image_get_width  heif_image_get_height
heif_image_get_bits_per_pixel_range  heif_image_get_plane_readonly
heif_get_decoder_descriptors
```

Twenty-seven functions, all with C-ABI signatures over opaque pointers and `c_int` enums, plus one struct returned by value (`heif_error`: two `c_int` and a `const char *`) and one struct the library allocates and we write into (`heif_decoding_options`, versioned by its first field; the loader writes `convert_hdr_to_8bit` only when the library's version is 3 or higher). `heif_get_decoder_descriptors` for `heif_compression_HEVC` is the "recognised but cannot decode" check, so a Fedora box gets a precise message before any decode is tried. Library names and search order:

```
Linux    libheif.so.1, dlopen's default path plus SQZER_LIBHEIF
macOS    libheif.1.dylib, then /opt/homebrew/lib and /usr/local/lib
         explicitly because dyld's default search does not include them
Windows  heif.dll (vcpkg's name), libheif.dll (msys2's), on PATH
```

Version gate: `heif_get_version_number() >= 0x01_11_00_00`, the 1.17 floor ADR-0004 already set. Estimated size: about 350 lines in `heif-dl` (declarations, the loader struct, the two safe entry points) and about 100 lines of changes in `heif.rs` to read planes from `Raw` instead of `libheif-rs` types; the colour, alpha and widening logic stays. `libloading` is ISC, MSRV 1.88, and it is the crate `wgpu`, `ash` and the Vulkan loaders use for this job.

Where it does not work: a statically linked musl binary. musl's `dlopen` in a static executable returns null with "dynamic loading not supported", by design, so the loader is compiled out for `target_env = "musl"` and the musl release artifacts have no HEIC, which the README states next to the other musl gaps.

Alongside, not instead. The loader alone would cover Linux and Homebrew Macs, with one conformance surface and no new mapping code, which is attractive. It would also leave every Mac without Homebrew and every Windows machine without HEIC, and those are the machines the HEIC files are on. The OS decoders cost two small crates and the verification list in this record; that is the price of "download and it works" on the two platforms where it can.

---

## 3. Options considered

**Keep link-time `libheif-rs` for source builds next to the loader.** Packagers get automatic dependency tracking (`dpkg-shlibdeps`, Homebrew's `depends_on`) from a link-time dependency, and `libheif-rs` is maintained by someone else. Rejected for now: two backends for one library is two sets of bugs for a solo maintainer, `cargo-dist`'s run-stage Homebrew dependency covers the one packager this project has, and the loader is a strict superset of what the link-time path can do for a binary. Revisit if a distribution packages `sqzer` and asks.

**Apply orientation through the OS.** ImageIO's thumbnail-with-transform and WIC's `IWICBitmapFlipRotator` both work. Rejected because it makes orientation a per-backend behaviour again, and the thumbnail path fixes the pixel format to premultiplied BGRA. Parsing `irot` and `imir` once, in safe Rust, gives the same answer on every platform and hands the portable tier correct dimensions for free.

**A WIC probe that only enumerates.** `IWICImagingFactory::CreateComponentEnumerator` or `MFTEnumEx` costs less than a decode and would let `--list-codecs` skip COM initialisation of a decoder. Rejected: neither proves that the HEIF codec and the HEVC MFT work together on this machine, which is the only thing the listing is asked. The decode probe runs once and only when HEIC is in play.

**`imageio` 0.11 (MIT OR Apache-2.0), a safe ImageIO wrapper.** It carries a Swift bridge and its own `apple-cf` layer. Rejected: a Swift toolchain at build time is a bigger dependency than the `unsafe` blocks it would save, and `objc2-image-io` is the binding the rest of the ecosystem has settled on.

**Delay-load and weak linking.** See D1. Platform-specific, and useless on Windows where the linked `libheif` has no HEVC decoder.

---

## 4. Trade-offs

**Three decoders for one format.** The conformance rule in D5 and the shared orientation path in D6 are what keep this from being three formats in a trench coat. The fixtures are the contract; a platform decoder that drifts fails CI on that platform.

**Unsafe code in the workspace.** Three crates opt out of `forbid(unsafe_code)`. Each is small, has one purpose, and is reviewed as a binding. `sqzer-core`, `sqzer-codecs`, `sqzer-metrics`, `sqzer` and the CLI stay under `forbid`.

**A Windows CI gap that may be permanent.** If GitHub's Windows runners never carry the Store extensions, the WIC decode path is tested by hand. The "unavailable" path is tested by CI, and it is the path most Windows machines take at least once.

**A probe that decodes.** `--list-codecs` on Windows does real work the first time. Tens of milliseconds once per process, only when the WIC backend is compiled in, is the accepted cost of a truthful listing.

**Verification debt.** Six behaviours in D2, D3 and D5 are marked "to verify". None of them changes the decision; each changes a line of mapping code. They are action item 6 and gate marking this record Accepted.

---

## 5. Consequences

What becomes easier: `cargo-dist` can run with `features = ["native"]` on all six desktop targets with no system library at build time, which unblocks ADR-0001 item 9. The Windows CI job builds the whole native tier. A HEIC on a machine without a decoder gets a message that names the fix. A portable build recognises HEIC, reports its dimensions and says which feature reads it.

What becomes harder: three backends to keep conformant instead of one, a hand-maintained FFI surface for `libheif` that has to track its `heif_decoding_options` versions, and two OS APIs whose HEIC behaviour is under-documented and was pinned down here partly from other projects' bug trackers.

What to revisit:

- The WIC 10-bit question, once measured. If the HEIF codec offers `64bppRGBA` for 10-bit sources, the deviation in D5 disappears.
- The Windows runner. A `windows-latest` image that ships the HEIF extension, or a self-hosted runner, turns the by-hand decode tests into CI.
- `libheif`'s own plugin loading. Newer `libheif` finds decoders as plugins at runtime; if the runtime loader can also point it at a bundled `libde265` plugin directory, the Fedora case gets a fix instead of a message. Out of scope here.

---

## 6. Action items

1. [x] `sqzer-core`: `Format::decoder_features`, `Error::DecoderUnavailable`, `Decoder::available`; the registry skips unavailable decoders in `probe`; `--list-codecs` prints the reason. Portable HEIC sniff and ISOBMFF walk (`ispe`, `irot`, `imir`), orientation applied through `Image::apply_orientation` by every HEIC backend.
2. [x] `heif-dl`: the loader crate of D8 over `libloading`; `heif.rs` moved onto it; `libheif-rs` and `libheif-sys` removed; the pkg-config and vcpkg steps removed from CI and the README; musl compiled out.
3. [x] `heif-imageio`: the ImageIO backend of D2, registered before the loader on macOS; CI on both macOS runners with the loader tested separately.
4. [x] `heif-wic`: the WIC backend of D3 with the decode probe, registered before the loader on Windows; the Windows CI job on the full `native` set; a scratch job that runs `Get-AppxPackage Microsoft.HEIFImageExtension, Microsoft.HEVCVideoExtension*` on `windows-latest` to settle what the runner can do.
5. [x] `pattern-10bit.heic` in `tests/fixtures`, made as the other five were, documented in the fixtures README.
6. [ ] Verify on real machines and record the answers in the backend docs: ImageIO's data-provider alpha kind and monochrome layout, whether the P3 `prof` bytes round-trip through `CGColorSpaceCopyICCData`, the HRESULT WIC returns for a `.heic` with the HEIF package and without HEVC, whether WIC offers more than 8 bits for a 10-bit source, and the probe's cost in milliseconds.
7. [ ] Mark this record Accepted, then close ADR-0001 item 9 with the `cargo-dist` configuration, including `[dist.dependencies.homebrew] libheif = { stage = ["run"] }` so the formula pulls `libheif` for the loader.

Implementation notes, 2026-09-11, branch `feat/heic-os-decoders`. Where the code departs from the text above:

- `libheif` keeps applying `irot` and `imir` itself. Its decoding options can only skip every transform together, and skipping `clap` would hand back the coded frame with its padding, so the loader leaves the transforms on. The container walk is still the single source of the orientation value: the OS backends apply it, and the rotated fixture is checked on every backend.
- The ICC profile comes from the container walk (`colr` of type `prof` or `rICC`) for all three backends, so `heif-dl` binds 24 functions rather than the 27 of D8: the three colour-profile calls are not needed.
- A `heif_init` that fails to load a plugin is not fatal. The HEVC decoder count decides availability, and a `libheif` with none reports "no HEVC decoder plugin" with the plugin error appended.
- `Error::DecoderUnavailable` and the `--list-codecs` listing carry the reason of every compiled-in backend, joined, not only the first.
- Item 4's scratch job is two steps of the native Windows job: `Get-AppxPackage` before the build and `--list-codecs` after it. Items 3 and 4 are written against the crate sources and cross-checked with `cargo check` and `cargo clippy` for `aarch64-apple-darwin` and `x86_64-pc-windows-msvc` from Linux; their first real run is CI.

---

## Sources

- [Cykooz/libheif-sys on GitHub](https://github.com/Cykooz/libheif-sys), `Cargo.toml` metadata `package.metadata.vcpkg.target.x86_64-pc-windows-msvc` (triplet `x64-windows-static-md`, port `libheif[aom]`) and `build.rs` (pkg-config via `system-deps`, embedded build, vcpkg; no runtime loading)
- [czkawka issue 810: error while loading shared libraries: libheif.so.1](https://github.com/qarmin/czkawka/issues/810)
- [cargo-dist configuration reference](https://github.com/axodotdev/cargo-dist/blob/main/book/src/reference/config.md): `features`, `precise-builds`, `[dist.dependencies]` with `stage` and `targets`; no per-target features
- [HEIF extension codec, Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/wic/heif-codec): pixel formats, `MFTEnumEx` as the codec presence check
- [WIC GUIDs and CLSIDs](https://learn.microsoft.com/en-us/windows/win32/wic/-wic-guids-clsids): `CLSID_WICHeifDecoder`
- [Native pixel formats overview](https://learn.microsoft.com/en-us/windows/win32/wic/-wic-codec-native-pixel-formats): `64bppRGBAHalf`, `32bppRGBA1010102`
- [paint.net forum: ComponentNotFoundException 0x88982F50 on CreateDecoderFromStream](https://forums.getpaint.net/topic/120617-file-broken-with-this-error-message/)
- [ImageGlass issue 1928: 9.0 ignores HEIC orientation](https://github.com/d2phap/ImageGlass/issues/1928)
- [ghosthack/imageio-native](https://github.com/ghosthack/imageio-native): WIC flip-rotator from metadata, the 8 x 8 HEVC extension minimum, ImageIO thumbnail-with-transform
- [MindGems: WIC codecs for HEIC, HEIF, AVIF](https://www.mindgems.com/article/wic-codecs-heic-heif-avif/): HEIF Image Extension needs HEVC Video Extensions for `.heic`
- [Microsoft Q&A: HEIF Image Extensions blocked by Store policy](https://learn.microsoft.com/en-us/answers/questions/5853241/unable-to-install-heif-image-extensions-heic-codec)
- [Windows Central: HEIC and HEVC support on Windows 11](https://www.windowscentral.com/software-apps/windows-11/how-to-add-support-for-heic-and-hevc-files-on-windows-11)
- [Apple: Using HEIF or HEVC media on Apple devices](https://support.apple.com/en-us/116944): built into macOS 10.13
- [MacRumors: HEVC in macOS High Sierra](https://www.macrumors.com/guide/hevc-video-macos-high-sierra-ios-11/): hardware on 2016 and later Macs, software elsewhere
- [Apple forums: check if a device can decode HEIC](https://developer.apple.com/forums/thread/129662)
- [Apple forums: NSBitmapImageRep on a 10-bit HEIF](https://developer.apple.com/forums/thread/700647): 10 bits per component, 40 bits per pixel
- [Apple forums: 10-bit HEIC to 16-bit PNG with Image I/O](https://developer.apple.com/forums/thread/672561): unresolved
- [Quartz 2D: supported pixel formats for bitmap contexts](https://developer.apple.com/library/archive/documentation/GraphicsImaging/Conceptual/drawingwithquartz2d/dq_context/dq_context.html)
- [CGImageSourceCopyTypeIdentifiers](https://developer.apple.com/documentation/imageio/1465383-cgimagesourcecopytypeidentifiers)
- [rustc platform support: Apple Darwin](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html): 10.12 on x86_64, 11.0 on aarch64
- [objc2-image-io on crates.io](https://crates.io/crates/objc2-image-io), [objc2-core-graphics](https://crates.io/crates/objc2-core-graphics), [objc2-core-foundation](https://crates.io/crates/objc2-core-foundation): 0.3.2, Zlib OR Apache-2.0 OR MIT, MSRV 1.71
- [madsmtm/objc2 issue 266: availability](https://github.com/madsmtm/objc2/issues/266): no per-OS-version features, tested on 10.12
- [windows on crates.io](https://crates.io/crates/windows): 0.62.2, MIT OR Apache-2.0, MSRV 1.82; [windows-core](https://crates.io/crates/windows-core) 0.100 asks for 1.95
- [microsoft/windows-rs releases](https://github.com/microsoft/windows-rs/releases)
- [libloading on crates.io](https://crates.io/crates/libloading): 0.9.0, ISC, MSRV 1.88
- [imageio on crates.io](https://crates.io/crates/imageio): 0.11.0, Swift bridge
- [strukturag/libheif issue 471: premultiplied alpha](https://github.com/strukturag/libheif/issues/471)
- [libheif advisory GHSA-jc8f-p23p-5hjg](https://github.com/strukturag/libheif/security/advisories/GHSA-jc8f-p23p-5hjg): `clap` double application in the tiling API, fixed in 1.23.1
- [rpmfusion/libheif-freeworld](https://github.com/rpmfusion/libheif-freeworld): HEVC for Fedora's libheif
- [Debian: heif-gdk-pixbuf](https://packages.debian.org/bookworm/heif-gdk-pixbuf): the GdkPixbuf loader is libheif
- [FFmpeg-devel: tile HEIF still images in avformat/mov](https://patchwork.ffmpeg.org/project/ffmpeg/patch/20240209222817.13543-2-jamrial@gmail.com/)
- [musl mailing list: static linking and dlopen](https://musl.openwall.narkive.com/lW4KCyXd/static-linking-and-dlopen)
- [MSVC linker: delay-loaded DLLs](https://learn.microsoft.com/en-us/cpp/build/reference/linker-support-for-delay-loaded-dlls?view=msvc-170); [rustup's build.rs](https://github.com/rust-lang/rustup/blob/main/build.rs) for the `/delayload` + `delayimp.lib` pattern in Cargo
