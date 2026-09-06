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
(rotate 90 degrees clockwise) brings it upright. `pattern-rgb16.jxl` and
`pattern-10bit.avif` hold the 8-bit values widened by replication.

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
```

The ICC profile in the `-icc` files is a Display P3 profile synthesised by
`jxl-color` 0.9.0; the same bytes are embedded in all three containers so a
test can compare them. The EXIF blob is a minimal little-endian TIFF with a
single `Orientation` entry.
