#!/usr/bin/env bash
# Everything CI's `linux` job would fail you for, in one command.
#
# The list has been in docs/development.md since the beginning and running it by
# hand still goes wrong the same way every time: format, make one more edit,
# commit, and watch CI fail on formatting twenty minutes later. One command
# cannot be half-run.
#
# It formats rather than only checking, because a formatting difference is never
# a decision -- then it checks, so a file that could not be formatted still
# fails here rather than on CI.
#
# Use --fast while a measurement VM is running: the workspace test run spawns
# many test binaries at once, and together with a 5 GB virtual machine it was
# enough for this machine to start reclaiming memory and for background tasks to
# be culled. Nothing was lost, but the full run is not free company.
#
# Usage: scripts/check.sh [--fast]      --fast skips the workspace test run
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
fast=${1:-}

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

step "format"
cargo fmt --all
cargo fmt --all -- --check

step "clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

if [[ $fast != --fast ]]; then
  step "workspace tests"
  cargo test --workspace --locked
fi

step "scripts"
# /usr/bin/python3 on purpose: a version manager's python3 has no PyGObject,
# and these import the Files extension.
python=$(command -v /usr/bin/python3 || command -v python3)
"$python" scripts/smoke.py
"$python" scripts/test-observe-service.py
"$python" scripts/test-install-tray-autostart.py
"$python" scripts/test-install-scripts.py
"$python" scripts/test-nautilus-extension.py
"$python" scripts/acceptance-ledger.py

step "docs"
cargo doc --workspace --no-deps --locked >/dev/null

printf '\n\033[1mall checks passed\033[0m\n'
echo "Not covered here: the window scenarios, which need a display --"
echo "  cargo test -p cirrove-desktop --test window --locked -- --ignored --test-threads=1"
