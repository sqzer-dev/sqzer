#!/usr/bin/env bash
# Writes the third-party notices of one build of sqzer: the notice of
# every crate in its dependency tree, through `cargo about` with
# `about.toml` and `about.hbs` at the repository root, then every PATENTS
# file a crate ships, a grant that travels with the licence and that
# `cargo about` has no slot for. `crates/sqzer-wasm/pack.sh` runs it for
# the npm package; the release build of `release.yml` runs it for each
# archive, through `.github/dist-build-setup.yml`; the native job of
# `ci.yml` runs it for its target and checks the result.
#
#   tools/notices.sh <manifest> <target> <features> <output>
#   tools/notices.sh crates/sqzer-cli/Cargo.toml x86_64-unknown-linux-gnu native THIRD-PARTY-LICENSES.txt
#
# `features` is the feature list to resolve the tree with, "" for the
# defaults. Needs `cargo-about` and `jq`.
set -euo pipefail
manifest=$1
target=$2
features=$3
out=$4
root=$(cd "$(dirname "$0")/.." && pwd)
feature_args=()
if [ -n "$features" ]; then
  feature_args=(--features "$features")
fi

# `--fail` refuses a crate whose licence cannot be read. A stale
# clarification in `about.toml`, a crate whose notice moved with a bump,
# is only a warning to `cargo about`, which then drops that notice and
# goes on; here a warning fails the run, so nothing is left out without
# anyone knowing. Its "warning: config found for a crate not present in
# the graph" is lowercase and expected: `about.toml` covers the npm
# package and the binaries, and no one tree holds every crate it names.
log=$(cargo about generate --fail --locked -c "$root/about.toml" -m "$manifest" \
  --target "$target" "${feature_args[@]}" -o "$out" "$root/about.hbs" 2>&1) || {
  echo "$log" >&2
  exit 1
}
if grep -q WARN <<< "$log"; then
  echo "$log" >&2
  echo "cargo about warned: a notice is missing, or a clarification in about.toml is stale" >&2
  exit 1
fi

# Every PATENTS file shipped by a crate of the tree, by name, four levels
# deep so the third-party code inside a vendored C tree is covered. The
# tree is the one `cargo about` saw: `cargo tree` for the target, over
# normal and build dependencies, never dev ones. `cargo metadata` only
# maps a crate to its directory: its own resolve enables the features of
# every platform at once, so on musl it would add the JPEG XL crates the
# musl binary does not carry. A path crate, printed with its directory,
# is ours and is not shipped. One such file, `rav1e`'s, is Windows-1252;
# the output stays UTF-8.
{
  echo "================================================================================"
  echo "Patent grants"
  echo
  echo "Additional rights granted with a licence above, as the crate ships them."
} >> "$out"
declare -A dir_of
while IFS=$'\t' read -r crate dir; do
  dir_of[$crate]=$dir
done < <(cargo metadata --format-version 1 --locked --manifest-path "$manifest" "${feature_args[@]}" |
  jq -r '.packages[] | select(.source != null)
    | "\(.name) \(.version)\t\(.manifest_path | sub("[/\\\\]Cargo\\.toml$"; ""))"')
cargo tree --locked --manifest-path "$manifest" --target "$target" "${feature_args[@]}" \
  -e normal,build --prefix none --format '{p}' |
  awk '$3 == "" || $3 == "(*)" || $3 == "(proc-macro)" { sub(/^v/, "", $2); print $1 " " $2 }' |
  sort -u |
  while read -r crate; do
    dir=${dir_of[$crate]:-}
    if [ -z "$dir" ]; then
      echo "no directory known for $crate" >&2
      exit 1
    fi
    # A Windows path from `cargo metadata`, for the Git Bash `find`.
    if command -v cygpath > /dev/null; then
      dir=$(cygpath -u "$dir")
    fi
    find "$dir" -maxdepth 4 -type f -iname 'PATENTS*' | sort | while read -r file; do
      printf '\n--------------------------------------------------------------------------------\n%s  %s\n\n' "$crate" "${file#"$dir"/}"
      if iconv -f utf-8 -t utf-8 "$file" > /dev/null 2>&1; then
        cat "$file"
      else
        iconv -f windows-1252 -t utf-8 "$file"
      fi
      echo
    done
  done >> "$out"
test -s "$out"
