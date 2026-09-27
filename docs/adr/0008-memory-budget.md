# ADR-0008: A memory budget for `--jobs`

**Status:** Proposed
**Date:** 2026-09-27
**Deciders:** Vlad (sole maintainer)
**Scope:** What bounds the number of files in flight in the CLI. Adds a memory limit next to the decoded-pixel rule of ADR-0003 "Parallelism and memory", which stays as a second limit. Nothing here changes `--jobs`, `--threads` or `--max-pixels` as flags, or the per-file parallelism of ADR-0001 D3.

---

## 1. Context

ADR-0003 bounded `-j` with a decoded-pixel budget: a file reserves its header pixel count and waits while the total would pass `max_pixels * jobs / 4`. That rule counts decoded samples, a few bytes per pixel. The default path does much more than decode. The SSIMULACRA2 search keeps a precomputed `fast-ssim2` reference and a compare context, multi-scale `f32` XYB, `mu` and `sigma` planes for both sides, and that dominates the peak.

A folder of seven iPhone HEICs, 5712x4284 each, took down a WSL2 VM with 7.7 GB of memory. With 12 CPUs, `jobs` is 7 and the pixel budget is `268M * 7 / 4 = 469M` pixels. The folder is 171M pixels, so all seven files ran their searches at once.

Peak memory of one file, `-j 1`, release binary with `native`, measured as the cgroup's `Memory peak` under `systemd-run`:

```text
# 24,470,208 pixels
-n (decode only)          152.3M     6 bytes/pixel
-f jpeg -q 80             165.9M     7
-f webp -q 80             231.5M     9
-f png --lossless         258.7M    11
-f jxl -q 80              472.9M    19
-f avif -q 60             523.2M    21
-f jpeg   (target 70)       3.2G   131
-f jxl    (target 70)       3.3G   135
default   (avif, target 70) 3.3G   135
```

The cost splits into three tiers: decode only, one encode, and the search. The encoder barely matters under a search. Seven searches at once need about 23 GB.

## 2. Decision

### D1. The budget is bytes, sized from available memory

At the start of a run the CLI reads the memory available to the process: the system's available memory through `sysinfo`, lowered to the tightest cgroup headroom above the process where there is one. The budget is three quarters of it. The quarter left over covers the snapshot going stale and the error in the estimates.

`sysinfo` reads a cgroup limit only at the root of `/sys/fs/cgroup`. That is the process's own cgroup inside a container, but not in a limited cgroup on the host. On Linux the CLI also reads `/proc/self/cgroup` and walks the process's cgroup v2 and its ancestors, taking the lowest `memory.max` minus `memory.current`:

```text
# budget as seen from inside the unit
systemd-run --user -p MemoryMax=4G ...     3.21 GB
```

A cgroup at its limit has zero headroom. That is a reading, not a missing one: the budget is zero bytes and files run one at a time.

The ADR-0003 pixel rule stays as a second limit. Each file reserves its decoded pixels next to its bytes and waits while either total would pass its limit. A lowered `--max-pixels` has always limited how many files run at once, and a large machine must not admit more than it did, however far a resize shrinks the byte estimates: `--max-pixels 30M` with eight jobs still admits two 24-megapixel files. Where no memory figure is available, the byte limit is unbounded and the pixel rule stands alone, so behaviour there is what it was.

### D2. Each file reserves an estimate of its peak

Before decoding, a worker reads the header dimensions and reserves:

```text
decoded pixels * 8  +  output pixels * work
work: 0 for --dry-run, 24 for one encode, 152 for the search
```

Output pixels are after `--max-width` / `--max-height`, so a resized run searches, and reserves, fewer pixels. The header is read before EXIF orientation, which may swap the sides, and a one-sided bound then fits a different box: the estimate takes the larger of the two fits. Constants carry headroom over the measurements: decode 8 against 6, one encode 32 in total against 21, the search 160 in total against 135.

The run decides `work` once, from the flags: `-n` is decode only, a perceptual target without `--fast` is the search, everything else is one encode. A format picked per image may be lossless-only and skip the search. The estimate stays on the safe side of that.

A file whose dimensions cannot be read reserves the estimate for a whole `--max-pixels` image, so it runs alone, as before.

### D3. A file over the budget on its own runs alone, with a warning

The reservation logic is unchanged: a request larger than the whole budget is granted once nothing else is running. The CLI now also prints a warning, unless `--quiet`:

```text
warning: big/huge.png: 12000x12000 needs about 23.04 GB, more than the 5.45 GB this run may use; it runs alone
  the target search is most of that; -q sets an explicit quality and skips it
```

Refusing the file would be worse. The estimate is an upper bound, and swap or a larger cgroup may well carry it.

## 3. Options considered

**Keep the pixel rule, lower the divisor.** No divisor is right for both a 3 MB PNG folder at `-q` and a folder of camera files at the default target: they differ by 20x per pixel.

**Measure the process's resident memory while running.** Reactive, platform-specific, and the peak arrives inside the metric in one step, too late to hold a file back.

**Downscale the metric for large images.** Changes the product: a score at a different resolution is a different target. Worth its own ADR if single huge images turn out to matter.

**Read memory by hand per OS.** `/proc/meminfo`, `sysctl` and `GlobalMemoryStatusEx` are small, but the Windows and macOS calls need `unsafe`, which the workspace forbids outside the binding crates. `sysinfo` 0.38 (MIT) with only the `system` feature does it. 0.39 needs Rust 1.95, above the workspace MSRV of 1.92. The cgroup walk of D1 is plain file reads, so it stays in the CLI.

## 4. Trade-offs

The constants are measurements of today's backends on one photo. A backend that needs more than 24 bytes per pixel for one encode, or a `fast-ssim2` change to the search's working set, makes the estimate low. The test `estimate_orders_the_work_and_follows_the_resize` pins the relation to the measured 3.3 GB. It does not measure the allocation itself, since that would take a counting global allocator, which needs `unsafe`.

The cgroup walk covers cgroup v2 only. A cgroup v1 host outside a container sees the host's available memory; `sysinfo` still covers v1 inside a container.

On an 8 GB machine a folder of 24-megapixel photos now runs one search at a time, where the old rule ran all of them and failed. On a 64 GB machine it runs up to about twelve.

## 5. Consequences

- `PixelBudget` becomes `MemoryBudget` in `crates/sqzer-cli/src/budget.rs`, reserving a `Cost` of bytes and decoded pixels, with `Work`, `output_pixels` and `available_memory`. `Config` gains `work`.
- `Printer` gains `warning`.
- `sysinfo` joins the CLI's dependencies. The library and the wasm build do not change.
- The `-j` help text, the README's memory paragraph and the changelog describe the new rule.

## 6. Action items

1. [x] Byte budget, estimate and warning, with unit tests for the limit, zero headroom, the pixel-rule cap under a resize, the fallback, the estimate's order, the resize in either orientation and the cgroup walk.
2. [x] Verify on the seven HEICs under a 5.5 GiB cap: all seven written, one at a time, 3.5 GB peak. Under a 4 GiB cap each file warns and runs alone, 3.4 GB peak.
3. [ ] Re-measure the constants when a backend is added or `fast-ssim2` is bumped; the numbers of section 1 are the baseline.

---

## Sources

- [sysinfo on crates.io](https://crates.io/crates/sysinfo), [GuillaumeGomez/sysinfo](https://github.com/GuillaumeGomez/sysinfo)
- [fast-ssim2 on crates.io](https://crates.io/crates/fast-ssim2)
