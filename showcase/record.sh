#!/usr/bin/env bash
# Regenerate showcase/img/*.png, one screenshot per built-in theme. Each shot
# is the review screen with all three panes up: the file sidebar, the diff, and
# the comments sidebar, so a theme is judged on everything it has to colour.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
img="$root/showcase/img"
repo="$(mktemp -d)/showcase-repo"
trap 'rm -rf "$(dirname "$repo")"' EXIT

( cd "$root" && cargo build --release -p diffler >/dev/null )
diffler="$root/target/release/diffler"
mkdir -p "$img" "$repo/src" "$repo/tests"

cd "$repo"
git init -q
git config user.email reviewer@example.invalid
git config user.name reviewer
git config commit.gpgsign false

cat > src/auth.rs <<'RS'
use crate::error::ApiError;

pub struct Claims {
    pub subject: String,
    pub scopes: Vec<String>,
}

/// Authenticate a request from its bearer token.
pub fn authenticate(headers: &Headers) -> Result<Claims, ApiError> {
    let token = headers.get("authorization").and_then(strip_bearer);
    let token = token.ok_or(ApiError::Unauthorized)?;
    if token == expected_token() {
        Ok(decode_claims(&token))
    } else {
        Err(ApiError::Unauthorized)
    }
}
RS
cat > README.md <<'MD'
# api

Bearer authentication for the public API.
MD
git add -A
git commit -qm "auth module"

cat > src/auth.rs <<'RS'
use crate::error::ApiError;
use subtle::ConstantTimeEq;

pub struct Claims {
    pub subject: String,
    pub scopes: Vec<String>,
}

/// Authenticate a request from its bearer token.
pub fn authenticate(headers: &Headers) -> Result<Claims, ApiError> {
    let token = headers.get("authorization").and_then(strip_bearer);
    let token = token.ok_or(ApiError::Unauthorized)?;
    // constant-time compare so a mismatch cannot leak the prefix by timing
    if verify_bearer(&token) {
        Ok(decode_claims(&token))
    } else {
        Err(ApiError::Unauthorized)
    }
}
RS
cat > tests/auth_test.rs <<'RS'
#[test]
fn a_mismatched_token_is_rejected() {
    assert!(!verify_bearer("wrong"));
}
RS
cat >> README.md <<'MD'

Tokens are compared in constant time.
MD

# both anchors sit on changed lines: a comment on unchanged context has no row
# in the diff and renders as outdated
anchor_line="$(grep -n 'if verify_bearer(&token) {' src/auth.rs | cut -d: -f1)"
anchor_text="$(sed -n "${anchor_line}p" src/auth.rs)"
dep_line="$(grep -n 'use subtle::ConstantTimeEq;' src/auth.rs | cut -d: -f1)"
dep_text="$(sed -n "${dep_line}p" src/auth.rs)"

mkdir -p .diffler/reviews
# what diffler writes on its first save: the review state is not part of the
# diff being reviewed
printf '*\n' > .diffler/.gitignore
python3 - "$anchor_line" "$anchor_text" "$dep_line" "$dep_text" <<'PY'
import json, sys

anchor_line, anchor_text, dep_line, dep_text = (
    int(sys.argv[1]), sys.argv[2], int(sys.argv[3]), sys.argv[4]
)


def anchor(path, line, text):
    return {
        "file": path,
        "line": line,
        "line_end": None,
        "on_old_side": False,
        "line_text": text,
    }


review = {
    "version": 1,
    "comments": [
        {
            "id": "c1",
            "author": "reviewer",
            "anchor": anchor("src/auth.rs", anchor_line, anchor_text),
            "body": "Use `verify_bearer` here, good. Make sure it does a "
                    "**constant-time** compare:\n\n```rust\n"
                    "fn verify_bearer(token: &str) -> bool {\n"
                    "    expected().as_bytes().ct_eq(token.as_bytes()).into()\n"
                    "}\n```\n\n"
                    "| Path | Leaks timing |\n"
                    "|---|---|\n"
                    "| `==` on bytes | yes, on the first differing byte |\n"
                    "| `ct_eq` | no, every byte is compared |\n",
            "status": "replied",
            "replies": [
                {
                    "author": "agent",
                    "body": "Done. `verify_bearer` uses `subtle::ConstantTimeEq`, "
                            "so it compares in constant time. Added a test for the "
                            "mismatch path too.",
                    "at": 1,
                }
            ],
            "at": 1,
        },
        {
            "id": "c2",
            "author": "reviewer",
            "anchor": anchor("src/auth.rs", dep_line, dep_text),
            "body": "Is `subtle` already a dependency here, or does this pull "
                    "a new crate into the build?",
            "status": "open",
            "replies": [],
            "at": 2,
        },
        {
            "id": "c3",
            "author": "reviewer",
            "anchor": anchor("tests/auth_test.rs", 3, "    assert!(!verify_bearer(\"wrong\"));"),
            "body": "Worth a case for a token of the right length too.",
            "status": "open",
            "replies": [],
            "at": 3,
        },
    ],
    "viewed": {},
}
open(".diffler/reviews/working.json", "w").write(json.dumps(review, indent=2))
PY

# `--seed` leaves the review repo on disk and records nothing, so the screen
# the tape is about to shoot can be inspected without nine renders
if [[ "${1:-}" == "--seed" ]]; then
    trap - EXIT
    echo "$repo"
    exit 0
fi

for name in github-dark catppuccin-mocha tokyo-night gruvbox-dark nord rose-pine kanagawa dracula github-light; do
    tape="$(mktemp).tape"
    {
        echo "Output \"$img/_discard.gif\""
        echo "Set Shell bash"
        echo "Set FontSize 15"
        echo "Set Width 1500"
        echo "Set Height 900"
        echo "Set Padding 0"
        echo "Hide"
        echo "Type \"cd $repo && clear\"" ; echo "Enter" ; echo "Sleep 400ms"
        echo "Type \"$diffler --theme $name\"" ; echo "Enter" ; echo "Sleep 1800ms"
        echo "Type \"D\"" ; echo "Sleep 800ms"
        # the comments sidebar, then step onto the answered thread: its
        # selection drives the diff pane onto the line it anchors to
        echo "Type \"C\"" ; echo "Sleep 900ms"
        echo "Type \"j\"" ; echo "Sleep 600ms"
        echo "Type \"k\"" ; echo "Sleep 1400ms"
        echo "Show"
        echo "Sleep 300ms"
        echo "Screenshot \"$img/$name.png\""
        echo "Sleep 500ms"
        echo "Type \"qq\""
    }> "$tape"
    vhs "$tape"
    rm -f "$tape"
    echo "  $name"
done
rm -f "$img/_discard.gif"
ls "$img"
