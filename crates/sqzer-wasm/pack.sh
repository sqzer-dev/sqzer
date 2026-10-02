#!/usr/bin/env bash
# Builds the npm package `sqzer` from this crate and proves the tarball
# works: what `.github/workflows/publish-npm.yml` publishes from a tag
# (ADR-0011 D5), and what the wasm job of `ci.yml` rehearses on every pull
# request. Leaves `pkg/sqzer-<version>.tgz`.
#
# Needs `wasm-pack`, `clang`, `jq` and Node 22.14 or later.
set -euo pipefail
cd "$(dirname "$0")"

# A `RUSTFLAGS` variable would replace the wasm32 flags of
# `.cargo/config.toml`, `simd128` among them.
rm -rf pkg
env -u RUSTFLAGS wasm-pack build --release --target web --out-name sqzer

# What `wasm-pack` 0.15 gets wrong for this package. It names it after the
# crate. It leaves `snippets/` out of `files`, so the tarball would lack
# the JavaScript of `decodeAny` that `sqzer.js` imports. And it looks for
# the licence texts in the crate directory; they are at the repository
# root.
cp ../../LICENSE-MIT ../../LICENSE-APACHE pkg/
jq '.name = "sqzer" | .files += ["snippets", "LICENSE-MIT", "LICENSE-APACHE"]' \
  pkg/package.json > pkg/package.json.new
mv pkg/package.json.new pkg/package.json

(cd pkg && npm pack --silent)
tarball=$(realpath pkg/sqzer-*.tgz)

# Install the tarball the way a consumer does and run it. An import that
# resolves to a file the tarball lacks fails here, not on npm.
consumer=$(mktemp -d)
trap 'rm -rf "$consumer"' EXIT
cp tests/package.mjs "$consumer/"
fixtures=$(realpath ../../tests/fixtures)
(
  cd "$consumer"
  npm init --yes > /dev/null
  npm install --silent "$tarball"
  node package.mjs "$fixtures"
)
echo "packed $tarball"
