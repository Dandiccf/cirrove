#!/usr/bin/env bash
# Build and test the optional KF6 plugins when their development files exist.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
build=${CIRROVE_DOLPHIN_BUILD_DIR:-$repo/.local-build/dolphin-check}

if ! find /usr/lib /usr/lib64 /usr/local/lib -maxdepth 4 \
  -name KF6KIOConfig.cmake -print -quit 2>/dev/null | grep -q .; then
  echo "Dolphin plugins not built: KF6 KIO development files are not installed"
  exit 0
fi

cmake -S "$repo/packaging/dolphin" -B "$build" \
  -DCMAKE_BUILD_TYPE=RelWithDebInfo -DBUILD_TESTING=ON
cmake --build "$build" --parallel
ctest --test-dir "$build" --output-on-failure
