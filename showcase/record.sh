#!/usr/bin/env bash
# Regenerate showcase/img/*.png, one screenshot per built-in theme.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
img="$root/showcase/img"
repo="$(mktemp -d)/showcase-repo"
trap 'rm -rf "$(dirname "$repo")"' EXIT

( cd "$root" && cargo build --release -p diffler >/dev/null )
diffler="$root/target/release/diffler"
mkdir -p "$img" "$repo/src"

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

anchor_line="$(grep -n 'if verify_bearer(&token) {' src/auth.rs | cut -d: -f1)"
anchor_text="$(sed -n "${anchor_line}p" src/auth.rs)"

mkdir -p .diffler/reviews
python3 - "$anchor_line" "$anchor_text" <<'PY'
import json, sys
line, text = int(sys.argv[1]), sys.argv[2]
review = {
    "version": 1,
    "comments": [
        {
            "id": "c1",
            "author": "reviewer",
            "anchor": {
                "file": "src/auth.rs",
                "line": line,
                "line_end": None,
                "on_old_side": False,
                "line_text": text,
            },
            "body": "Use `verify_bearer` here, good. Make sure it does a "
                    "**constant-time** compare, not `==` on the raw bytes, or a "
                    "mismatch leaks the prefix by timing.",
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
        }
    ],
    "viewed": {},
}
open(".diffler/reviews/working.json", "w").write(json.dumps(review, indent=2))
PY

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
        echo "Type \"l\"" ; echo "Sleep 300ms"
        echo "Type \"jjjjjjjj\"" ; echo "Sleep 1500ms"
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
