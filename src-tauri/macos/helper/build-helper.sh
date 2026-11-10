#!/usr/bin/env bash
set -euo pipefail

# Builds Swift privileged helper (NSXPCListener) from Xcode project
# and outputs path to resulting helper Mach-O binary.
#
# Usage:
#   LABEL="ru.ravel.ultunnel-macos.helper" OUT_DIR=/tmp/out ./build-helper.sh
#   ARCH=x86_64 LABEL="ru.ravel.ultunnel-macos.helper" ./build-helper.sh
#   ./build-helper.sh ru.ravel.ultunnel-macos.helper
#
# Environment:
#   SCHEME (default: UltunnelPrivilegedHelper)
#   CONFIGURATION (default: Release)
#   ARCH (optional; e.g. x86_64, arm64, or "arm64 x86_64")
#   SIGN_IDENTITY (optional; signing is done later by package-macos.sh anyway)

LABEL="${LABEL:-${1:-}}"
if [[ -z "${LABEL}" ]]; then
  echo "LABEL is required. Example: LABEL=\"ru.ravel.ultunnel-macos.helper\" $0" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="${OUT_DIR:-$PWD}"
mkdir -p "$OUT_DIR"

OUT_HELPER="$OUT_DIR/$LABEL"
OUT_PLIST="$OUT_DIR/$LABEL.plist"

# Xcode project locations
PROJ_DIR="$SCRIPT_DIR/UltunnelPrivilegedHelper"
PROJECT="$PROJ_DIR/UltunnelPrivilegedHelper.xcodeproj"

if [[ ! -d "$PROJECT" ]]; then
  echo "Missing Xcode project: $PROJECT" >&2
  exit 1
fi

SCHEME="${SCHEME:-UltunnelPrivilegedHelper}"
CONFIGURATION="${CONFIGURATION:-Release}"
ARCH="${ARCH:-}"

ARCH_ARGS=()
if [[ -n "$ARCH" ]]; then
  ARCH_ARGS+=(
    "ARCHS=$ARCH"
    "ONLY_ACTIVE_ARCH=NO"
  )
fi

LAUNCHD_PLIST="$SCRIPT_DIR/ru.ravel.ultunnel-macos.helper.plist"
if [[ ! -f "$LAUNCHD_PLIST" ]]; then
  echo "Missing launchd plist: $LAUNCHD_PLIST" >&2
  exit 1
fi

DERIVED_DATA="$(mktemp -d -t ultunnel-helper-derived.XXXXXX)"
cleanup() { rm -rf "$DERIVED_DATA"; }
trap cleanup EXIT

# Build helper (do NOT rely on Xcode signing here; we sign later inside the .app bundle)
xcodebuild \
  -project "$PROJECT" \
  -scheme "$SCHEME" \
  -configuration "$CONFIGURATION" \
  -derivedDataPath "$DERIVED_DATA" \
  "${ARCH_ARGS[@]}" \
  CODE_SIGNING_ALLOWED=NO \
  OTHER_LDFLAGS="\$(inherited) -Wl,-sectcreate,__TEXT,__launchd_plist,\"$LAUNCHD_PLIST\"" \
  build \
  | tee /tmp/ultunnel_helper_build.log

PRODUCTS_DIR="$DERIVED_DATA/Build/Products/${CONFIGURATION}"

# Try to locate Mach-O produced by helper target.
# Prefer exact name match; otherwise pick first Mach-O in products root.
HELPER_BUILT=""
if [[ -f "$PRODUCTS_DIR/$LABEL" ]]; then
  HELPER_BUILT="$PRODUCTS_DIR/$LABEL"
else
  HELPER_BUILT="$(find "$PRODUCTS_DIR" -maxdepth 2 -type f -print0 \
    | xargs -0 file 2>/dev/null \
    | grep -E 'Mach-O (64-bit|universal) executable' \
    | head -n 1 \
    | sed -E 's/:.*$//' || true)"
fi

if [[ -z "${HELPER_BUILT:-}" || ! -f "$HELPER_BUILT" ]]; then
  echo "Cannot find built helper Mach-O in: $PRODUCTS_DIR" >&2
  echo "Tip: check scheme name (SCHEME=$SCHEME) and products output." >&2
  exit 1
fi

# Copy helper to requested output with required install name (LABEL)
cp -f "$HELPER_BUILT" "$OUT_HELPER"
chmod 755 "$OUT_HELPER" || true

# Optional: copy launchd plist next to the helper.
if [[ -f "$SCRIPT_DIR/ru.ravel.ultunnel-macos.helper.plist" ]]; then
  cp -f "$SCRIPT_DIR/ru.ravel.ultunnel-macos.helper.plist" "$OUT_PLIST"
elif [[ -f "$PROJ_DIR/Helper/ru.ravel.ultunnel-macos.helper.plist" ]]; then
  cp -f "$PROJ_DIR/Helper/ru.ravel.ultunnel-macos.helper.plist" "$OUT_PLIST"
fi

# Validate requested architecture(s) before packaging.
if [[ -n "$ARCH" ]]; then
  ACTUAL_ARCHS="$(lipo -archs "$OUT_HELPER" 2>/dev/null || true)"
  if [[ -z "$ACTUAL_ARCHS" ]]; then
    echo "ERROR: cannot determine helper architectures: $OUT_HELPER" >&2
    file "$OUT_HELPER" >&2 || true
    exit 1
  fi
  for REQUIRED_ARCH in $ARCH; do
    if ! printf '%s\n' "$ACTUAL_ARCHS" | tr ' ' '\n' | grep -Fxq "$REQUIRED_ARCH"; then
      echo "ERROR: helper architecture mismatch" >&2
      echo "  required: $ARCH" >&2
      echo "  actual:   $ACTUAL_ARCHS" >&2
      exit 1
    fi
  done
fi

# Validate embedded sections required by SMJobBless
if ! otool -l "$OUT_HELPER" | grep -q "__info_plist"; then
  echo "ERROR: __info_plist is NOT embedded in helper binary: $OUT_HELPER" >&2
  echo "The helper target must embed Info.plist via -sectcreate __TEXT __info_plist ..." >&2
  exit 1
fi
if ! otool -l "$OUT_HELPER" | grep -q "__launchd_plist"; then
  echo "ERROR: __launchd_plist is NOT embedded in helper binary: $OUT_HELPER" >&2
  echo "The helper target must embed launchd plist via -sectcreate __TEXT __launchd_plist ..." >&2
  exit 1
fi

echo "Built helper: $OUT_HELPER" >&2
file "$OUT_HELPER" >&2
echo "$OUT_HELPER"
