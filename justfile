default:
    @just --list

# install the binary to ~/.cargo/bin
install:
    cargo install --path crates/diffler --locked

# build and run against a repo (defaults to the current directory)
run *args:
    cargo run -p diffler -- {{args}}

# fast inner-loop verification (agents: run after every change)
check:
    cargo clippy --workspace --all-targets --all-features

test:
    cargo nextest run --workspace --all-features
    cargo test --doc --workspace

# auto-fix what's mechanical
fix:
    cargo clippy --workspace --all-targets --all-features --fix --allow-dirty --allow-staged
    cargo fmt --all

fmt:
    cargo fmt --all

# snapshot tests: review .snap.new diffs before accepting
snap:
    cargo insta test

snap-accept:
    cargo insta accept

# cut a release: bump the version (Cargo + npm in lockstep), gate, tag, push;
# CI then builds binaries and publishes crates.io + npm from the committed version
release-patch:
    bash scripts/release.sh patch

release-minor:
    bash scripts/release.sh minor

release-major:
    bash scripts/release.sh major

# PTY end-to-end suite (CI runs this in a separate ubuntu job)
e2e:
    cargo build -p diffler
    uv run --with pexpect --with pyte --with pytest --with mcp pytest tests/e2e -x -q

# core gate, matches CI's test+lint jobs (CI additionally runs msrv, deny, typos)
ci:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo nextest run --workspace --all-features
    cargo test --doc --workspace
