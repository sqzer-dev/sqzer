# Test fixtures

Small, redistributable test images. Keep the whole folder under 2 MB;
anything larger belongs in the calibration corpus, which is downloaded, not
committed.

## The pattern

Every `pattern-*` file encodes the same 48 x 32 synthetic image, defined in
`crates/sqzer-codecs/tests/common/mod.rs` as `test_image`. The tests
regenerate it in code, so lossless files are checked for exact equality and
lossy ones for a bounded mean absolute error. No reference PNG is needed.

```
r = x * 255 / 47            horizontal ramp
g = y * 255 / 31            vertical ramp
b = 40 if x < 24 else 220   hard vertical edge
a = 255 on even rows, 128 on odd rows
gray = (r + g + b) / 3
```

`pattern-rot90.*` store the pattern rotated so that EXIF orientation 6
(rotate 90 degrees clockwise) brings it upright. `pattern-rgb16.jxl`,
`pattern-10bit.avif` and `pattern-10bit.heic` hold the 8-bit values widened
by replication.

## How they were made

Generated once from a throwaway Rust program, not committed, with these
crates and settings. Regenerate with the same to keep the tests meaningful.

```
# JPEG: mozjpeg-rs 0.9.2
pattern-rgb.jpg           ProgressiveBalanced, q92, 4:2:0
pattern-gray.jpg          BaselineFastest, q92, encode_gray
pattern-icc.jpg           ProgressiveBalanced, q92, 4:4:4, ICC profile embedded
pattern-rot90.jpg         ProgressiveBalanced, q92, 4:4:4, EXIF orientation 6

# WebP: image-webp 0.2.4 (lossless), libwebp 1.x via the `webp` crate (lossy)
pattern-rgb.webp          lossless RGB
pattern-rgba.webp         lossless RGBA
pattern-icc.webp          lossless RGB, ICCP chunk
pattern-rot90.webp        lossless RGB, EXIF chunk with orientation 6
pattern-lossy.webp        libwebp q92
pattern-lossy-alpha.webp  libwebp q92 with alpha
pattern-anim.webp         hand-assembled VP8X + ANIM + two ANMF frames (lossless,
                          no blending); frame 2 is the pattern flipped vertically

# JPEG XL: jxl-encoder 0.3.1 (AGPL-3.0, used only as a tool; the files are data)
pattern-rgb.jxl           lossless, effort 3
pattern-rgba.jxl          lossless, effort 3
pattern-gray.jxl          lossless, effort 3
pattern-rgb16.jxl         lossless 16-bit, effort 3
pattern-lossy.jxl         VarDCT, distance 1.0, effort 4 (XYB)
pattern-icc.jxl           lossless with an embedded ICC profile
pattern-container.jxl     lossless in the ISOBMFF container (has an Exif box)

# AVIF: rav1e 0.8.1 + avif-serialize 0.8.9, ravif 0.13.0 for the alpha file
pattern-rgb.avif          8-bit 4:2:0, BT.709, limited range, quantizer 40, speed 9
pattern-10bit.avif        10-bit 4:2:0, BT.709, full range, quantizer 40, speed 9
pattern-gray.avif         8-bit monochrome, full range
pattern-rgba.avif         ravif q90, alpha q90, speed 8 (4:4:4 plus alpha item)

# HEIC: libheif 1.19.8 + x265 4.1 through libheif-rs 3.0.0 (x265 is GPL, used only
# as a tool; the files are data). All 8-bit 4:2:0, quality 92, brand `heic`. Every
# file is coded as 64 x 64 with a `clap` box cropping it to the pattern.
pattern-rgb.heic          YCbCr
pattern-rgba.heic         YCbCr plus an alpha auxiliary image
pattern-gray.heic         monochrome
pattern-icc.heic          YCbCr with the Display P3 profile in a `prof` colr box
pattern-rot90.heic        stored rotated so that the container's `irot` (90 degrees
                          clockwise) brings it upright; the header reports the
                          displayed 48 x 32
pattern-10bit.heic        10-bit 4:2:0, quality 92, the same way with x265 4.3; the
                          conformance fixture of ADR-0005 for what each HEIC decoder
                          does with a deep source

# GIF: image 0.25.10's GifEncoder (NeuQuant quantisation) for the RGB file, the
# gif 0.14.2 crate for the other two. The pattern has more colours than a palette
# holds, so every GIF is quantised and the tests compare within a bound
pattern-rgb.gif           one frame filling the screen, no transparent index
pattern-alpha.gif         one frame, odd rows transparent
pattern-anim.gif          two frames: the pattern, then a 16 x 16 solid patch at (8, 8)

# TIFF: tiff 0.11.3
pattern-rgb.tif           RGB8, uncompressed
pattern-rgba.tif          RGBA8, LZW
pattern-gray.tif          Gray8, uncompressed
pattern-rgb16.tif         RGB16, uncompressed
pattern-rot90.tif         RGB8 stored rotated, Orientation tag 6
pattern-icc.tif           RGB8 with the Display P3 profile in tag 34675

# BMP, TGA, ICO, QOI, PNM: image 0.25.10's encoders, except the 16-bit PPM,
# which was written by hand (P6, maxval 65535, big-endian)
pattern-rgb.bmp           24-bit
pattern-rgba.bmp          32-bit
pattern-rgb.tga           RLE true colour
pattern-rgba.tga          RLE true colour with alpha
pattern-gray.tga          RLE 8-bit gray
pattern-rgba.ico          one 48 x 32 entry, PNG-compressed
pattern-rgb.qoi           RGB
pattern-rgba.qoi          RGBA
pattern-rgb.ppm           P6, binary
pattern-ascii.ppm         P3, ASCII
pattern-gray.pgm          P5, binary
pattern-rgb16.ppm         P6, maxval 65535

# SVG: written by a script, one 1 x 1 rect per pixel on integer coordinates
# with shape-rendering="crispEdges", so the rasterised result is the pattern
# exactly
pattern-rgb.svg           width and height attributes plus a viewBox
pattern-rgba.svg          viewBox only; odd rows at fill-opacity 0.50196 (128/255)

# OpenEXR: ImageMagick 7.1.2 from the PNM and QOI fixtures, `-set colorspace
# sRGB -colorspace RGB -depth 16`, so the pattern in linear light as half
# floats, uncompressed. Alpha is written straight, not premultiplied.
pattern-rgb.exr           R, G, B
pattern-rgba.exr          R, G, B, A
pattern-gray.exr          Y only, `-define exr:color-type=Y`
```

```
# metadata: exiftool 13.55 over the rotated fixtures, so EXIF orientation 6,
# EXIF Artist "sqzer" and an XMP packet with dc:creator "sqzer" and dc:title
# "test pattern" sit beside stored-rotated pixels
pattern-meta.jpg          pattern-rot90.jpg plus the tags
pattern-meta.webp         pattern-rot90.webp plus the tags, EXIF and XMP chunks
pattern-meta.tif          pattern-rot90.tif plus the tags; the EXIF is the file's own IFD, XMP is tag 700
pattern-meta.jxl          cjxl 0.12 --lossless_jpeg=1 --compress_boxes=0 from pattern-rot90.jpg,
                          so the codestream carries the orientation and the Exif box repeats it;
                          the tags added by exiftool after
```

The ICC profile in the `-icc` files is a Display P3 profile synthesised by
`jxl-color` 0.9.0; the same bytes are embedded in all three containers so a
test can compare them. The EXIF blob is a minimal little-endian TIFF with a
single `Orientation` entry.
