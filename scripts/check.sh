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
# Usage: scripts/check.sh [--fast]      --fast skips the workspace tests and
#                                       the ones that mount
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

  # The tests that mount. CI runs these and this script did not, which is how a
  # change that made pinning asynchronous reached CI twice in one day: the
  # `real_` prefix is exactly the set that `cargo test` leaves out, and four of
  # them assert what is on disk after a pin. They need /dev/fuse and
  # fusermount3; a machine without them is told so rather than quietly skipping
  # the only tests that drive the product through a kernel.
  if [[ -w /dev/fuse ]] && command -v fusermount3 >/dev/null; then
    step "kernel mounts"
    # The environment each group needs, from .github/workflows/ci.yml. The
    # reclamation fixture retains a flat 16.5 MiB and cannot reach the shipped
    # 64 MiB floor; the window fixture measures RSS and would measure glibc
    # arena retention instead.
    CIRROVE_RECLAIM_FLOOR_BYTES=8388608 CIRROVE_RECLAIM_INTERVAL_SECONDS=1 \
      cargo test -p cirrove-service --test read_only --locked real_ \
        -- --ignored --test-threads=1
    cargo test -p cirrove-service --test google_drive --locked real_ \
      -- --ignored --test-threads=1
    cargo test -p cirrove-service --lib --locked filesystem::capacity::real_ \
      -- --ignored --test-threads=1 --skip real_writable_namespace_retires_and_stays_bounded
    MALLOC_ARENA_MAX=1 \
      cargo test -p cirrove-service --lib --locked content::windows::tests::graph::kernel::real_ \
        -- --ignored --test-threads=1
    cargo test -p cirrove-service --test writable_session --locked real_ \
      -- --ignored --test-threads=1 --skip real_a_save_on_a_full_device_is_reported_as_a_device_and_not_a_budget
  else
    printf '\n\033[1mNOT RUN: the tests that mount.\033[0m /dev/fuse or fusermount3 is missing.\n'
    echo "  CI runs them; they are the ones that drive the product through a kernel."
  fi
fi

step "scripts"
# /usr/bin/python3 on purpose: a version manager's python3 has no PyGObject,
# and these import the Files extension.
python=$(command -v /usr/bin/python3 || command -v python3)
"$python" scripts/smoke.py
"$python" scripts/test-observe-service.py
"$python" scripts/test-install-tray-autostart.py
"$python" scripts/test-install-scripts.py
"$python" scripts/test-package-versions.py
"$python" scripts/test-nautilus-extension.py
"$python" scripts/test-file-manager-docs.py
scripts/check-dolphin.sh
"$python" scripts/test-icon-geometry.py
"$python" scripts/test-translations.py
"$python" scripts/acceptance-ledger.py

step "docs"
cargo doc --workspace --no-deps --locked >/dev/null

printf '\n\033[1mall checks passed\033[0m\n'
if [[ $fast == --fast ]]; then
  # Said loudly, because it has already cost a red CI run: on 2026-09-15 a day
  # of --fast let a test that knew pinning was synchronous reach CI, and this
  # line is the only place that could have caught it before the push.
  printf '\033[1mNOT RUN: the workspace test suite, nor the tests that mount.\033[0m --fast skipped both.\n'
  echo "  Run scripts/check.sh with no arguments before pushing."
fi
echo "Not covered here: the window scenarios, which need a display --"
echo "  cargo test -p cirrove-desktop --test window --locked -- --ignored --test-threads=1"
