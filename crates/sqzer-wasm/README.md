# sqzer

Multi-format image optimizer with best-in-class defaults, as a WebAssembly package. The same pipeline and defaults as the [`sqzer`](https://github.com/sqzer-dev/sqzer) command line: decode, convert to sRGB, resize, then search encoder quality until the output reaches a SSIMULACRA2 score.

```js
import init, { optimize } from "sqzer";
await init();

// AVIF for a photograph, lossless WebP for a graphic, searched to a score of 70
const out = optimize(bytes);
// or say what you want
const avif = optimize(bytes, { format: "avif", target: 60, width: 1600 });
new Blob([avif.bytes], { type: "image/avif" });
```

> **Note**: The package is synchronous and single-threaded, and a search on a large photo takes seconds. Run it in a Web Worker.

## What it reads and writes

```
decode   JPEG, PNG, WebP, AVIF, JPEG XL, GIF, TIFF, BMP, TGA, ICO, QOI, PNM, OpenEXR
         SVG, and HEIC where the browser reads it, through `decodeAny`
encode   JPEG (mozjpeg-rs), PNG (oxipng), AVIF (ravif), lossless WebP (image-webp)
```

> **Note**: Lossy WebP and JPEG XL cannot be written: no permissively licensed pure-Rust encoder exists for either. Asking for one throws an `EncoderUnavailable` error, it never falls back to another format. `codecs()` lists what the build carries.

## Decode once, encode many times

```js
import init, { decode, decodeAny, fromPixels } from "sqzer";
await init();

const image = decode(bytes);
image.width; image.height; image.format; image.alpha; image.animated;

// the resized image is kept between calls, so a quality slider only pays for the encode
const a = image.encode({ format: "jpeg", quality: 80, width: 1600 });
const b = image.encode({ format: "jpeg", quality: 60, width: 1600 });

// the image lives in the module's memory, which the garbage collector does not see
image.free();

// the same, with the browser's canvas for what `decode` cannot read. a vector
// image is drawn to fit inside the box given, anything else at its own size
const logo = await decodeAny(svgBytes, { width: 1600 });

// pixels from anywhere else: RGBA, 8 bits, sRGB, as `getImageData` returns them.
// takes { maxPixels } as a fourth argument, like `decode`
const drawn = fromPixels(imageData.data, imageData.width, imageData.height);
```

> **Note**: `decodeAny` reads an SVG on the page, not in a worker. Chrome, Firefox and Safari all refuse an SVG blob in `createImageBitmap` there, and `decodeAny` throws `DecoderUnavailable` with the browser's reason. Draw the SVG on the page and hand the pixels to `fromPixels` in the worker, as [sqzer.dev](https://github.com/sqzer-dev/sqzer.dev) does. HEIC on Safari works in both.

## Options

The keys are the flags of the command line in camel case, and the same rules hold between them. All are optional.

```
format        "jpeg" | "png" | "webp" | "avif" | "jxl". default: chosen per image
target        SSIMULACRA2 score to search for, up to 100. default 70
quality       0 to 100, no search. excludes target and lossless
lossless      lossless output
preset        "web" | "thumbnail" | "archive" | "lossless". the keys above override it
effort        0 to 10, slower and smaller
subsampling   "auto" | "444" | "422" | "420"
fast          one encode at the calibrated seed quality for the target, no search
keepIcc       keep the ICC profile instead of converting to sRGB
keepMetadata  keep EXIF and XMP instead of stripping them
width         target width. with the default fit, scales down to fit inside
height        target height
fit           "inside" | "cover" | "contain" | "fill" | "outside", as CSS object-fit
position      where cover crops and contain places: "center", "top-left", ...
background    padding of contain: "#rrggbb", "#rrggbbaa", "white", "black", "transparent"
scale         a factor instead of a box: 0.5 halves both sides
enlarge       allow scaling up. nothing scales up without it
filter        "lanczos3" | "mitchell" | "catmull-rom" | "bilinear" | "box" | "nearest"
maxPixels     refuse a larger image. default 24 million
codecOpts     backend options, { "jpeg:progressive": "false" }. codecs() lists the keys
onTrial       called per trial of the search with { n, max, quality, score }
```

> **Note**: `maxPixels` defaults to 24 million, not the 268 million of the command line. The target search needs about 160 bytes per pixel, and a WebAssembly module addresses 4 GiB.

## Results and errors

`optimize` and `encode` return the record `sqzer --json` prints, in camel case, with the encoded file added. Keys that do not apply are left out.

```js
{
  bytes: Uint8Array,
  format: "avif", backend: "ravif", tier: "portable",
  inputFormat: "jpeg", animated: false, width: 4000, height: 3000, alpha: false,
  content: "photo", outputWidth: 1600, outputHeight: 1200,
  quality: 62.5, lossless: false,
  target: 70, score: 70.3, reached: true, capped: false, iterations: 3,
  trials: [{ quality: 62, score: 69.1 }, ...]
}
```

Every function throws a `SqzerError`: an `Error` whose `kind` names the failure, and whose `availableIn` lists the `sqzer` Cargo features that have a missing backend.

```js
try {
  optimize(bytes, { format: "jxl" });
} catch (e) {
  e.kind;        // "EncoderUnavailable"
  e.availableIn; // ["native-jxl"]
  e.message;     // "no encoder for JPEG XL in this build (enable one of: native-jxl)"
}
```

```
UnknownFormat        the bytes are no image this package knows
DecoderUnavailable   a known format with no decoder here: SVG, HEIC. try decodeAny
EncoderUnavailable   a format this build cannot write
TooLarge             over maxPixels
InvalidInput         not a valid picture
InvalidParams        an option out of range, misspelt, or excluded by another
Unsupported          the encoder cannot do what was asked, lossless JPEG for one
Codec                a backend failed
Transform            the colour or resize stage failed
```

TypeScript types for all of it ship in the package.

## Requirements

Any browser with WebAssembly SIMD: Chrome 91, Firefox 89, Safari 16.4. The canvas path of `decodeAny` also needs `OffscreenCanvas`, which Firefox has since 105. Node runs everything but that canvas path, and needs the module's bytes handed to `init`.

```js
import { readFile } from "node:fs/promises";
import init, { optimize } from "sqzer";
await init({ module_or_path: await readFile(new URL(import.meta.resolve("sqzer/sqzer_bg.wasm"))) });
```

## Licence

MIT or Apache-2.0, at your option.
