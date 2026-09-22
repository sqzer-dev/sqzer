# ADR-0006: Release matrix on `cargo-dist`

**Status:** Accepted
**Date:** 2026-09-13
**Deciders:** Vlad (sole maintainer)
**Scope:** How a `v*` tag becomes release binaries: the tool, the targets, the one feature list every target is built with and what it means on each, the installers, the Homebrew tap, and what CI proves before a tag. Closes ADR-0001 action item 9. Nothing here changes the tier rules of ADR-0001 D2, the crate choices of ADR-0004 or the HEIC design of ADR-0005.

---

## 1. Context

ADR-0001 D6 planned "portable for every target, native for the six desktop targets" through `cargo-dist`. ADR-0004 built the native tier and left three gaps in CI: no C++ toolchain on musl (so no `native-jxl` and no `native-jpegli` there), `native-jpegli` unable to share a cmake generator with `native-jxl` on Windows, and `jpegli-sys` segfaulting on the first encode on the aarch64 Linux runner. ADR-0005 removed the last link-time system library, so a native binary now starts on every machine, and named this record as the one it unblocks.

What `cargo-dist` can and cannot do, verified against 0.33.0 (released 2026-09-11):

- One `features` list for the whole workspace, or per package. No per-target override. The feature list has to build on all six targets as it is.
- `[dist.dependencies.apt]`, `homebrew` and `chocolatey` entries take a `stage` (`build`, `run`) and a `targets` list. A run-stage Homebrew entry becomes `depends_on` in the generated formula. apt entries are installed on the runner before the build.
- `github-build-setup` names a file of workflow steps injected into every local build job after checkout, with the `matrix` and `runner` contexts available.
- Default runners: `ubuntu-22.04` for x86_64 Linux and musl, `ubuntu-22.04-arm` for aarch64 Linux (a native build, no cross toolchain), `macos-14`, `macos-15-intel`, `windows-2022`. `musl-tools` is installed for the musl target without being asked.
- The app is named after the Cargo package, so archives and installers carry `sqzer-cli-`; `formula` renames the Homebrew formula only.
- The project is `axodotdev/cargo-dist` again. axo wound down in 2025 and Astral kept a fork at 0.28 for its own tools; upstream resumed, the fork's README points back to it, and 0.29 to 0.33 are upstream releases.

What Cargo can and cannot do:

- No per-target features. A feature is the same list of dependencies and nested features on every target.
- A dependency may be declared in `[dependencies]` and again in one or more `[target.'cfg(...)'.dependencies]` tables; the entries that match the target are unified into one build with the union of their features. Each entry may be `optional`.
- One package cannot depend on the same package twice under different names. `cargo tree --target` tolerates it, `cargo metadata` refuses, and `dist` runs `cargo metadata`.

The question is therefore not which tool, but how one feature list can mean "every native backend" on x86_64 Linux and macOS and "every native backend that builds and runs here" on the other three, without lying on any of them.

---

## 2. Decision

**One release flavour, built with `--features native` on the six desktop targets, and `native` means the backends the target can build and run.**

```
target                   backends in the release binary          runner
x86_64 linux-gnu         webp jxl avif heif jpegli               ubuntu-22.04, apt nasm
aarch64 linux-gnu        webp jxl avif heif                      ubuntu-22.04-arm
x86_64 linux-musl        webp avif                               ubuntu-22.04, musl-tools, apt nasm
x86_64 apple-darwin      webp jxl avif heif jpegli               macos-15-intel, nasm 2 from nasm.us
aarch64 apple-darwin     webp jxl avif heif jpegli               macos-14
x86_64 windows-msvc      webp jxl avif heif                      windows-2022, setup-nasm
```

`heif` on musl builds and registers nothing (ADR-0005 D8); the row says so by leaving it out. Every other omission is one of ADR-0004's three gaps.

### D1. No portable artifact

Since ADR-0005 the native binary starts on every machine and does everything the portable build does, plus lossy WebP, JPEG XL, `libaom` AVIF, jpegli and HEIC input. A portable archive next to it would be a second build of the same six targets that can do less, and users would have to be told which one to download. The portable tier's release form is `cargo install sqzer-cli` and, once ADR-0001 item 10 lands, the wasm package. ADR-0001's "portable for every target" is met by those two, not by a second set of archives.

### D2. The per-target set is a crate, `sqzer-native-tier`

`sqzer/native` is `["dep:sqzer-native-tier", "sqzer-native-tier/native"]`. The new crate has no code. Its `native` feature makes `sqzer-codecs` a dependency, declared three times: unconditionally with `native-webp`, `native-avif` and `native-heif`; for every target but musl with `native-jxl`; for every target but musl, Windows and aarch64 Linux with `native-jpegli`. Cargo unifies the entries that apply into the one build of `sqzer-codecs`, so `sqzer-cli --features native` is the row above for whatever target it is built for, with no change to `dist-workspace.toml` and no change to `ci.yml`'s feature list per target.

The single `native-*` features of `sqzer` and `sqzer-cli` do not go through the crate. `sqzer-cli --features native-jpegli` on Windows still asks for jpegli, builds it alone (the generator clash is between jpegli and libjxl, not jpegli and MSVC), and on musl still fails in cmake for want of a C++ compiler. The umbrella is target-aware; a named backend is a promise or a build error, never silence.

`sqzer-codecs/native` keeps meaning all five. The backend crate states no target policy; the two crates people depend on do. The CLI's tests know the same rule through `crates/sqzer-cli/src/native_set.rs`, three `cfg` constants that mirror the manifest and are included by path into the integration tests.

### D3. Installers and names

Shell and PowerShell installers, a Homebrew formula, and the archives themselves. No MSI: it needs WiX metadata and an upgrade GUID that should be minted once and kept, and no one has asked for one. No npm installer for the CLI: npm is where the wasm package of ADR-0001 item 10 goes, and an npm `sqzer` that downloads a native binary would take the name from it.

The Cargo package is `sqzer-cli`, so the archives are `sqzer-cli-<target>.tar.xz` (`.zip` on Windows), the installers `sqzer-cli-installer.sh` and `.ps1`, and the installer's environment variables `SQZER_CLI_*`. The formula is renamed to `sqzer`, so Homebrew users type `brew install sqzer-dev/tap/sqzer`. The binary is `sqzer` everywhere. The same package-versus-binary split is how `ripgrep` ships `rg`; the README shows the commands so nobody has to know.

Each archive carries `README.md`, `CHANGELOG.md` and both licence files, which `dist` picks up from the workspace root. `[profile.dist]` inherits `release` unchanged: fat LTO, one codegen unit, stripped.

### D4. Homebrew tap and `libheif`

The tap is `sqzer-dev/homebrew-tap`. The publish job writes `Formula/sqzer.rb` there with a `HOMEBREW_TAP_TOKEN` secret (a fine-grained token with contents write on the tap repository) stored on this repository. The formula declares `depends_on "libheif"`, as ADR-0005 item 7 asked, so a Mac whose ImageIO cannot decode HEVC and every Linux Homebrew user get a library for the runtime loader without a second step. Nothing links it; a binary installed any other way starts without it.

Both prerequisites are manual and listed in the action items. Until they exist the `publish-homebrew-formula` job fails and the GitHub release still goes out: publishing runs after the release is created.

### D5. Build environment

`.github/dist-build-setup.yml` holds the steps the native CI job already needs, so the release builds with what CI tested:

```
CMAKE_POLICY_VERSION_MINIMUM=3.5        jpegli's vendored libjpeg-turbo under cmake 4
CC_x86_64_unknown_linux_musl=musl-gcc   libdeflate, libwebp, libaom on musl
CXX_x86_64_unknown_linux_musl=g++       libaom's cmake project wants a C++ compiler to exist
nasm 2.16.03 from nasm.us               x86_64 macOS: libaom 3.11 rejects brew's nasm 3
ilammy/setup-nasm                       Windows
apt nasm                                x86_64 Linux and musl, through [dist.dependencies.apt]
```

The runners are `dist`'s defaults. `ubuntu-22.04` sets the glibc floor of the Linux archives at 2.35, two releases lower than CI's `ubuntu-24.04`; its GCC 11 and cmake 3.22 satisfy every vendored library. The musl archive is fully static.

### D6. What CI proves before a tag

The native CI job now builds `sqzer-cli --features native` and runs its tests on all six targets, so every release row above is compiled and exercised on every push, not only on tag day. The `sqzer-codecs` tests keep their explicit per-target feature lists, which is where a divergence from the crate's tables would show up. The release workflow's `plan` job runs on every pull request and fails when `release.yml` no longer matches `dist-workspace.toml`, so the workflow is never edited by hand: edit the TOML, run `dist generate`, commit both.

The build jobs run on tags only. A rehearsal is `pr-run-mode = "upload"` on a branch, which builds and uploads the archives as workflow artifacts without a release; it is not left on because it would run the four-minute C build six times on every pull request.

---

## 3. Options considered

```
a  two flavours, portable and native       ADR-0001's text. cargo-dist cannot express
                                           it (one feature list), two Cargo packages
                                           would collide on the binary name, and the
                                           portable archive is strictly less capable
                                           since ADR-0005. Rejected, D1
b  the intersection as the feature list    native-webp,native-avif,native-heif builds
                                           everywhere. No JPEG XL encoder for the four
                                           targets that can have one. Rejected
c  cfg the gaps out inside sqzer-codecs    the shape native-heif already has on musl.
                                           Makes native-jpegli alone on Windows silent
                                           where it builds, and pins a runner's
                                           segfault into the backend crate. Rejected
                                           for d, which keeps the named features strict
d  a shim crate for the umbrella           chosen, D2
e  sqzer twice under two names in the CLI  the same effect with no new crate.
                                           cargo metadata refuses it; noted so nobody
                                           tries it again
f  a generic dist project with a script    build-command picks features per target.
                                           Loses the cargo integration (linkage report,
                                           source tarball, auditable) for a problem d
                                           solves in twenty lines of TOML. Rejected
```

Also considered and deferred: aarch64 Windows and aarch64 musl archives (neither is in CI, and a target the release builds but CI does not is a target nobody tests until it breaks), an MSI, an npm installer for the CLI, `cargo-auditable` and a CycloneDX SBOM (cheap to add, no one has asked), a self-updater (`install-updater`, off).

---

## 4. Trade-offs

**`native` means two things by layer.** All five in `sqzer-codecs`, the target's set in `sqzer` and `sqzer-cli`. The backend crate is not the one people depend on, and all three manifests say which meaning they carry. The alternative, one meaning everywhere, is either b or c above.

**The gaps are stated, not closed.** The musl archive has no JPEG XL encoder, no jpegli and no HEIC; the Windows and aarch64 Linux archives have no jpegli. `--list-codecs` reports what a binary carries and the README lists the same rows. One message is wrong on musl: an `-f jxl` there says "needs the `native-jxl` feature, or a native build from the releases page", and the user has that build. Making the hint target-aware is an action item, not a blocker.

**`libheif` in the formula.** Homebrew installs `libheif` and its decoder tree for a fallback ImageIO rarely needs. ADR-0005 item 7 asked for it; revisit if it becomes the complaint.

**Two copies of the target rule.** The crate's manifest and `native_set.rs` state the same three `cfg` lines. A test cannot read a manifest, so the copy stays, next to a comment saying so.

**`ubuntu-22.04` is untested until the first tag.** CI runs the C tree on 24.04. The older image is the right choice for the glibc floor and its toolchain meets every stated minimum; the first rehearsal build settles it.

---

## 5. Consequences

What becomes easier: a tag is a release. `cargo install sqzer-cli --features native` builds on all six targets. CI compiles and tests the exact release set on every push. Closing one of the three gaps is one line in `sqzer-native-tier`'s manifest, one in `native_set.rs`, and one in `ci.yml`'s list for `sqzer-codecs`.

What becomes harder: a new native backend with a gap on some target has to be added in the same three places, and the README's per-target rows kept honest.

What to revisit:

- The musl C++ gap. `cargo-zigbuild` ships a C++ compiler for musl and writes a cmake toolchain file; if libjxl and jpegli build under it, musl gets JPEG XL and jpegli and the row above grows.
- The jpegli gaps, when `jpegli-sys` finds its library under a multi-config generator and its libjxl tree stops crashing on Neoverse (ADR-0004 item 4).
- Signing: `dist` 0.33 added Azure Artifact Signing for Windows binaries. Worth it once there are Windows users hitting SmartScreen.

---

## 6. Action items

1. [x] `sqzer-native-tier`; `sqzer/native` through it; `native_set.rs` and the CLI tests target-aware; the native CI job on `--features native` for `sqzer` and `sqzer-cli` on all six targets.
2. [x] `dist-workspace.toml`, `.github/dist-build-setup.yml`, `.github/workflows/release.yml` generated by `dist` 0.33.0, `[profile.dist]`; the formula named `sqzer` with `depends_on "libheif"`; README install section and per-target rows.
3. [x] Before the first tag: create the `sqzer-dev/homebrew-tap` repository (empty, public), mint `HOMEBREW_TAP_TOKEN` with contents write on it and add the secret to this repository. (Both done 2026-09-16; the token is fine-grained and expires in September 2027, after which only the Homebrew publish job fails.)
4. [x] Rehearse once with `pr-run-mode = "upload"` on a branch and check `sqzer --list-codecs` from each of the six archives against the table in section 2. (PR 13, 2026-09-16: all six built on the default runners. The two Linux archives were run: the gnu binary lists all five backends, links only `libstdc++`, `libgcc_s`, `libm` and `libc` and needs glibc 2.35; the musl binary is fully static and lists `webpx` and `libavif` with JPEG on `mozjpeg-rs`, no JPEG XL encoder and no HEIC, as the table says. Both encode. Archive sizes: 4.2 to 6.1 MB. The Mac and Windows binaries were built and packaged but not run.)
5. [ ] The first tag is `v0.1.0`, not `v0.0.1`: bump the workspace version, turn `## Unreleased` in `CHANGELOG.md` into `## 0.1.0` (`dist` takes the release notes from there), merge, tag. What "polished enough for 0.1.0" means is decided outside this record.
6. [x] Make the `EncoderUnavailable` hint target-aware, so the musl archive does not send its user to the releases page. (`native_set::left_out` in the CLI: a native build that leaves a backend out on its target says why and which archive carries it, in the `-f` refusal, in `--list-codecs` and in the HEIC decode error.)
7. [ ] Try `cargo-zigbuild` for the musl C++ backends in a separate branch; if it works, move `native-jxl` and `native-jpegli` onto musl in `sqzer-native-tier`, `native_set.rs` and `ci.yml` together.

---

## Sources

- [cargo-dist configuration reference](https://axodotdev.github.io/cargo-dist/book/reference/config.html): `features`, `precise-builds`, `dependencies` with `stage` and `targets`, `github-build-setup`, `github-custom-runners`, `pr-run-mode`, `formula`, `install-updater`
- [cargo-dist: customizing GitHub Actions](https://axodotdev.github.io/cargo-dist/book/ci/customizing.html): build setup steps, custom runners with `host` and `container`, default runner images
- [cargo-dist: Homebrew installer](https://axodotdev.github.io/cargo-dist/book/installers/homebrew.html): `tap`, `publish-jobs`, `HOMEBREW_TAP_TOKEN`, `formula`
- [axodotdev/cargo-dist releases](https://github.com/axodotdev/cargo-dist/releases): v0.33.0 on 2026-09-11, v0.32.0, v0.31.0
- [astral-sh/cargo-dist README](https://github.com/astral-sh/cargo-dist): "an unofficial fork of axodotdev/cargo-dist 0.28.0", "the upstream project is active again"
- [Cargo reference: platform specific dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#platform-specific-dependencies) and [renaming dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#renaming-dependencies-in-cargotoml)
- [Cargo reference: feature unification](https://doc.rust-lang.org/cargo/reference/features.html#feature-unification)
