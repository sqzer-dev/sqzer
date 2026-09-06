//! AGPL-3.0 codec backends (the zen* family): `zenwebp`, `zenjpeg`, `zenavif`,
//! `zenjxl`, `heic`.
//!
//! Reserved and empty in v1 (docs/adr/0001-system-design.md, D2). This crate
//! is never a dependency of `sqzer`, `sqzer-codecs`, or `sqzer-cli`; a user
//! opts in by depending on it directly and registering its backends into a
//! [`sqzer_codecs::Registry`](../sqzer_codecs/struct.Registry.html).
//!
//! `cargo deny` excludes this crate from the permissive licence allow-list
//! (see `deny.toml`) but still forbids it from being pulled in transitively
//! by any other crate in this workspace.
