//! Which backends a `native` build carries on this target.
//!
//! The `cfg` lines mirror the target tables in
//! `crates/sqzer-native-tier/Cargo.toml`; change both together. The unit
//! tests use this module through `crate::native_set`, `tests/cli.rs`
//! includes the file by path.

/// `gamut-jxl` is in the set everywhere but musl.
pub const JXL: bool = cfg!(all(feature = "native", not(target_env = "musl")));

/// `jpegli` is out on Windows, aarch64 Linux and musl.
pub const JPEGLI: bool = cfg!(all(
    feature = "native",
    not(any(
        windows,
        target_env = "musl",
        all(target_arch = "aarch64", target_os = "linux")
    ))
));

/// A HEIC decoder is registered everywhere but musl, where a static
/// binary cannot load `libheif` (ADR-0005 D8).
pub const HEIC: bool = cfg!(all(feature = "native", not(target_env = "musl")));

/// Whether this is a native build: what the releases page ships.
pub const NATIVE: bool = cfg!(feature = "native");

/// Why a native build on this target leaves `feature` out, and which
/// archive has it, for the user who already downloaded a release and must
/// not be sent back to the releases page (ADR-0006 item 6). `None` for a
/// feature this build carries, for one no target carries, and for a
/// portable build, where the feature itself is the answer.
pub fn left_out(feature: &str) -> Option<&'static str> {
    if !NATIVE {
        return None;
    }
    match feature {
        "native-jxl" if !JXL => Some(
            "the static musl build has no C++ toolchain for libjxl yet; the x86_64 Linux gnu \
             archive carries JPEG XL",
        ),
        "native-jpegli" if !JPEGLI => Some(JPEGLI_WHY),
        "native-heif" if !HEIC => Some(
            "a static musl binary cannot load libheif; the x86_64 Linux gnu archive reads HEIC \
             through libheif when the machine has it",
        ),
        _ => None,
    }
}

#[cfg(windows)]
const JPEGLI_WHY: &str = "jpegli does not build next to libjxl with the Visual Studio cmake \
                          generator; JPEG is mozjpeg-rs on Windows";
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
const JPEGLI_WHY: &str = "jpegli crashes on aarch64 Linux; JPEG is mozjpeg-rs there";
#[cfg(not(any(windows, all(target_arch = "aarch64", target_os = "linux"))))]
const JPEGLI_WHY: &str = "the static musl build has no C++ toolchain for jpegli yet; JPEG is \
                          mozjpeg-rs there";
