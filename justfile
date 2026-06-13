default:
    @just --list

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
