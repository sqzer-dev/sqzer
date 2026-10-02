// The packed tarball, installed and loaded the way a consumer loads it.
// `pack.sh` copies this next to a `node_modules` that holds only the
// tarball and runs it with the fixtures directory as its argument. The API
// itself is tested in `src/tests.rs`; this is about the package: its name,
// its files, and that the module the tarball carries is the one that runs.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import init, { codecs, decode, decodeAny, optimize } from "sqzer";

const fixtures = process.argv[2];
const installed = (file) => new URL(import.meta.resolve(`sqzer/${file}`));

const manifest = JSON.parse(await readFile(installed("package.json")));
assert.equal(manifest.name, "sqzer");
assert.equal(manifest.license, "MIT OR Apache-2.0");
for (const file of ["LICENSE-MIT", "LICENSE-APACHE", "README.md", "sqzer.d.ts"]) {
  await readFile(installed(file));
}

await init({ module_or_path: await readFile(installed("sqzer_bg.wasm")) });

const bytes = new Uint8Array(await readFile(join(fixtures, "pattern-rgb.jpg")));
const out = optimize(bytes, { format: "avif" });
assert.equal(out.format, "avif");
assert.equal(out.reached, true);
assert.ok(out.bytes instanceof Uint8Array && out.bytes.length > 0);

const image = decode(out.bytes);
assert.deepEqual([image.width, image.height, image.format], [48, 32, "avif"]);
assert.equal(image.encode({ format: "png" }).format, "png");
image.free();

// `decodeAny` is the export whose JavaScript lives in `snippets/`. Node
// has no canvas, so an SVG is refused, by that JavaScript.
const svg = await readFile(join(fixtures, "pattern-rgb.svg"));
await assert.rejects(decodeAny(new Uint8Array(svg)), { kind: "DecoderUnavailable" });

assert.equal(codecs().find((codec) => codec.format === "jpeg").encoder.backend, "mozjpeg-rs");
console.log(`${manifest.name}@${manifest.version} works from its tarball`);
