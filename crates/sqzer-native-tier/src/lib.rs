//! The native tier of `sqzer` as its release binaries carry it.
//!
//! There is no code here. `sqzer/native` depends on this crate, and this
//! crate's `Cargo.toml` enables the `sqzer-codecs` backends the target can
//! build and run: all five on `x86_64` Linux and macOS, four on `aarch64`
//! Linux and Windows, three on musl. See the manifest for why each one is
//! out where it is, and ADR-0006 for the release matrix it serves.

#![no_std]
