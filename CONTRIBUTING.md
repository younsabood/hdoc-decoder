# Contributing

Bug reports, documentation fixes, and focused code changes are welcome. Keep
changes scoped to this decoder repository; it intentionally has no dependency
on the encoder crate.

## Before opening an issue

- Search existing issues for the same problem.
- Include the Rust version, operating system, and a minimal `.hdoc` reproduction
  when it is safe to share.
- Do not attach credentials, private documents, or generated build artifacts.

## Local checks

Install the Rust version declared in `Cargo.toml`, then run:

```powershell
cargo fmt --check
cargo check --all-targets --all-features
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo package --no-verify
```

## Pull requests

Describe the behavior change, public API impact, and checks you ran. Update the
relevant Markdown guide and Mermaid diagram when the file format or decoder
behavior changes. Keep binary fixtures out of pull requests unless they are
small and redistributable.
