# Contributing

Issues and PRs welcome.

```sh
just ci     # fmt + clippy + tests — must pass before any commit
just e2e    # PTY end-to-end suite (needs uv)
prek install  # git hooks, once
```

Rust 1.88+, `just`, `cargo-nextest`. TUI changes need TestBackend + insta
snapshot coverage (`just snap`, read the diff before accepting). Comments
explain why, never what. Commit messages: short, imperative, one line.
