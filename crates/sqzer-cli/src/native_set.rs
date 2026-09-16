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
