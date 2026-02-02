#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI_DIR="$(cd "$ROOT_DIR/../.." && pwd)"

DERIVED="$SRC_TAURI_DIR/macos/.deriveddata"

PROJECT="$ROOT_DIR/UltunnelHelperInstaller.xcodeproj"
SCHEME="UltunnelHelperInstaller"
CONFIG="Release"

xcodebuild \
  -project "$PROJECT" \
  -scheme "$SCHEME" \
  -configuration "$CONFIG" \
  -derivedDataPath "$DERIVED" \
  build

OUT_BIN="$DERIVED/Build/Products/$CONFIG/$SCHEME"
echo "$OUT_BIN"
