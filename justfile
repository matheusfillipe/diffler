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

# push the AUR package (run locally; uses your aur.archlinux.org ssh key)
aur-publish:
    bash scripts/aur-push.sh

# PTY end-to-end suite (CI runs this in a separate ubuntu job)
e2e:
    cargo build -p diffler
    uv run --with pexpect --with pyte --with pytest --with mcp pytest tests/e2e -x -q

# copy-paste duplication gate, matches CI's dupes job (config in .jscpd.json)
dupes:
    npx --yes jscpd@4 crates/

# unused-dependency gate, matches CI's machete job
machete:
    cargo machete

# coverage with the CI floor, matches CI's coverage job
cov:
    cargo llvm-cov nextest --workspace --summary-only --fail-under-lines 85

# core gate, matches CI's test+lint jobs (CI additionally runs msrv, deny, typos, dupes, machete, coverage)
ci:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo nextest run --workspace --all-features
    cargo test --doc --workspace

# diff-pipeline benches (criterion)
bench:
    cargo bench -p diffler-core --bench pipeline
