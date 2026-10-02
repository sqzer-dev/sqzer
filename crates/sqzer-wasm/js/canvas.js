// The browser's own decoders, for input the package has no decoder for
// (ADR-0011 D3): SVG, and HEIC where the browser reads it. `decodeAny`
// calls these, in this order. None of it runs inside the target search.

/**
 * Hand `bytes` to the browser's decoder. Resolves to something `drawImage`
 * takes: an `HTMLImageElement` for a vector image on the main thread, an
 * `ImageBitmap` otherwise.
 */
export async function open(bytes, mime) {
  // `bytes` is a view of the module's memory, valid until the first
  // `await`. The `Blob` copies it.
  const blob = new Blob([bytes], { type: mime });
  // An `<img>` is rasterised at the size it is drawn at. An `ImageBitmap`
  // of a vector image is pixels already, at the image's own size, and
  // scaling it blurs. A worker has no `<img>` and gets the bitmap, which
  // every browser refuses for an SVG as of 2026: the caller draws it on
  // the page and uses `fromPixels` instead.
  if (mime === "image/svg+xml" && typeof Image === "function") {
    const url = URL.createObjectURL(blob);
    try {
      const img = new Image();
      img.src = url;
      await img.decode();
      return img;
    } finally {
      URL.revokeObjectURL(url);
    }
  }
  if (typeof createImageBitmap !== "function") {
    throw new Error("this environment has no `createImageBitmap`");
  }
  return await createImageBitmap(blob);
}

/** The size `source` has on its own, as `[width, height]`. */
export function size(source) {
  return Uint32Array.of(
    source.naturalWidth ?? source.width,
    source.naturalHeight ?? source.height,
  );
}

/**
 * `source` drawn at `width` x `height`: RGBA, 8 bits, sRGB, alpha not
 * premultiplied. Releases `source`, whether or not the drawing worked.
 */
export function rasterise(source, width, height) {
  try {
    const context = new OffscreenCanvas(width, height).getContext("2d", {
      willReadFrequently: true,
    });
    context.drawImage(source, 0, 0, width, height);
    return context.getImageData(0, 0, width, height).data;
  } finally {
    close(source);
  }
}

/**
 * Release the decoded pixels of `source` now. An `ImageBitmap` holds them
 * until it is closed or collected; an `<img>` has nothing to close.
 */
export function close(source) {
  source.close?.();
}
