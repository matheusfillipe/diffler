#!/usr/bin/env bash
# A published crate carries only its own directory, so a file it reaches
# outside one compiles here and fails at publish. We check the rule directly
# because the binary crate cannot be packaged before its new library version
# is on the index.
set -euo pipefail
exec python3 "$(dirname "$0")/check_package_includes.py"
