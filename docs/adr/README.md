# Architecture decision records

One file per decision, numbered, never edited after acceptance. A superseded record gets a `Superseded by` line at the top and stays.

```
0001-system-design.md                    crate layout, codec tiers, pipeline, defaults policy, CLI
0002-libdeflate-in-the-portable-tier.md  the one C dependency in the portable tier, and why it is compiled out on wasm32
0003-cli-interface.md                    the full grammar of the `sqzer` binary: flags, paths, output placement, feedback, exit codes
0004-native-tier.md                      which crates back the `native-*` features, why not `jpegxl-rs`, jpegli as a fifth backend, what CI covers
0005-heic-through-os-decoders.md        HEIC in release binaries: ImageIO on macOS, WIC on Windows, `libheif` loaded at runtime elsewhere, never linked; what `--list-codecs` has to say
0006-release-matrix.md                   `cargo-dist` on the six desktop targets, one feature list, `native` as what the target can carry, the installers and the Homebrew tap
```

Template for new records: copy the headings from `0001` (Context, Decision, Options considered, Trade-offs, Consequences, Action items).
