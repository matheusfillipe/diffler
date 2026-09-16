#!/usr/bin/env bash
# A published crate carries only its own directory, so a file a crate reaches
# outside one compiles here and fails at publish, after the tag is public.
# The binary crate cannot be packaged locally to catch that, since the release
# bumps it and its library in lockstep and the new library version is not yet
# on the index, so the rule is checked directly. Test code is exempt: packaging
# verifies the library alone.
set -euo pipefail
exec python3 "$(dirname "$0")/check_package_includes.py"
