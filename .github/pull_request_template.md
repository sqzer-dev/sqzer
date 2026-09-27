<!-- In English, please. The title becomes the squash commit: `feat(codecs): ...`, `fix(cli): ...`, `docs: ...`. -->

## What and why

<!-- What changes, and the issue or discussion it comes from. -->

## New dependencies

<!-- Crate, version and licence, or "none". Every licence must be on the `deny.toml` allow-list. -->

## Checklist

- [ ] `CHANGELOG.md` has an entry under `## Unreleased`, if behaviour changes
- [ ] tests cover the change; golden scores are unchanged, or the PR says why they moved
- [ ] `cargo fmt`, `cargo clippy --all-features`, `cargo test`, `cargo doc` and `cargo deny check` pass locally
