#!/usr/bin/env bash
set -euo pipefail

TAURI_TARGET=""
APP_PATH=""

usage() {
  cat <<USAGE
Usage: $0 [--target <rust-target>] [--app <path-to-app>]

Examples:
  $0 --target x86_64-apple-darwin
  $0 --target aarch64-apple-darwin
  $0 --target universal-apple-darwin
  $0 --target x86_64-apple-darwin --app /path/to/ultunnel-desktop.app
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      TAURI_TARGET="${2:?Missing value for --target}"
      shift 2
      ;;
    --app)
      APP_PATH="${2:?Missing value for --app}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      # Backward compatibility: allow a single positional .app path.
      if [[ -z "$APP_PATH" && "$1" == *.app ]]; then
        APP_PATH="$1"
        shift
      else
        echo "Unknown argument: $1" >&2
        usage >&2
        exit 1
      fi
      ;;
  esac
done

# Map Rust/Tauri target to Xcode architecture(s) for the privileged helper.
HELPER_ARCH=""
EXPECTED_ARCHS=""
DMG_ARCH_SUFFIX=""
if [[ -n "$TAURI_TARGET" ]]; then
  case "$TAURI_TARGET" in
    x86_64-apple-darwin)
      HELPER_ARCH="x86_64"
      EXPECTED_ARCHS="x86_64"
      DMG_ARCH_SUFFIX="_x64"
      ;;
    aarch64-apple-darwin)
      HELPER_ARCH="arm64"
      EXPECTED_ARCHS="arm64"
      DMG_ARCH_SUFFIX="_aarch64"
      ;;
    universal-apple-darwin)
      HELPER_ARCH="arm64 x86_64"
      EXPECTED_ARCHS="arm64 x86_64"
      DMG_ARCH_SUFFIX="_universal"
      ;;
    *)
      echo "Unsupported macOS target: $TAURI_TARGET" >&2
      exit 1
      ;;
  esac
fi

# ===== User-configurable env =====
SIGN_IDENTITY="${SIGN_IDENTITY:-Developer ID Application: Petr Lomakin (ASMHMRKL3K)}"
HELPER_LABEL="${HELPER_LABEL:-ru.ravel.ultunnel-macos.helper}"

HELPER_ENTITLEMENTS_REL="${HELPER_ENTITLEMENTS_REL:-macos/helper/UltunnelPrivilegedHelper/UltunnelPrivilegedHelper.entitlements}"
APP_ENTITLEMENTS_REL="${APP_ENTITLEMENTS_REL:-macos/ultunnel.entitlements.plist}"

# Notarization (optional)
NOTARY="${NOTARY:-0}"
NOTARY_PROFILE="${NOTARY_PROFILE:-}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"   # src-tauri/
cd "$ROOT"

APP_ENTITLEMENTS="$ROOT/$APP_ENTITLEMENTS_REL"
if [[ ! -f "$APP_ENTITLEMENTS" ]]; then
  echo "Missing app entitlements: $APP_ENTITLEMENTS" >&2
  exit 1
fi

# ===== Cleanup any previously installed helper (same label) =====
sudo launchctl bootout system "/Library/LaunchDaemons/$HELPER_LABEL.plist" 2>/dev/null || true
sudo rm -f "/Library/LaunchDaemons/$HELPER_LABEL.plist" "/Library/PrivilegedHelperTools/$HELPER_LABEL" || true

# ===== Helpers =====
plist_set_or_add() {
  local plist="$1" keypath="$2" type="$3" value="$4"
  /usr/libexec/PlistBuddy -c "Set $keypath $value" "$plist" 2>/dev/null || \
  /usr/libexec/PlistBuddy -c "Add $keypath $type $value" "$plist"
}

plist_set_string() {
  local plist="$1" keypath="$2" value="$3"
  local esc
  esc="$(printf '%s' "$value" | sed 's/\\/\\\\/g; s/"/\\"/g')"
  /usr/libexec/PlistBuddy -c "Set $keypath \"$esc\"" "$plist" 2>/dev/null || \
  /usr/libexec/PlistBuddy -c "Add $keypath string \"$esc\"" "$plist"
}

plist_delete_key() {
  local plist="$1" keypath="$2"
  /usr/libexec/PlistBuddy -c "Delete $keypath" "$plist" 2>/dev/null || true
}

get_tauri_identifier() {
  local conf="$ROOT/tauri.conf.json"
  if [[ -f "$conf" ]] && command -v python3 >/dev/null 2>&1; then
    python3 - <<'PY'
import json
with open('tauri.conf.json', 'r', encoding='utf-8') as f:
    print(json.load(f).get('identifier', ''))
PY
    return 0
  fi
  echo ""
}

require_archs() {
  local file_path="$1"
  local required="$2"

  [[ -f "$file_path" ]] || {
    echo "Missing Mach-O: $file_path" >&2
    exit 1
  }

  local actual
  actual="$(/usr/bin/lipo -archs "$file_path" 2>/dev/null || true)"
  if [[ -z "$actual" ]]; then
    echo "Cannot read architectures from: $file_path" >&2
    /usr/bin/file "$file_path" >&2 || true
    exit 1
  fi

  local arch
  for arch in $required; do
    if ! printf '%s\n' "$actual" | tr ' ' '\n' | grep -Fxq "$arch"; then
      echo "Wrong architecture: $file_path" >&2
      echo "  required: $required" >&2
      echo "  actual:   $actual" >&2
      exit 1
    fi
  done

  echo "Architecture OK: $file_path ($actual)"
}

sign_code_file() {
  local file_path="$1"

  echo "Signing Mach-O: $file_path"
  /usr/bin/codesign \
    --force \
    --options runtime \
    --timestamp \
    --sign "$SIGN_IDENTITY" \
    "$file_path"

  /usr/bin/codesign \
    --verify \
    --strict \
    --verbose=2 \
    "$file_path"
}

sign_macho_under() {
  local dir="$1"
  local skip_path="${2:-}"
  [[ -d "$dir" ]] || return 0

  while IFS= read -r -d '' f; do
    if [[ -n "$skip_path" && "$f" == "$skip_path" ]]; then
      continue
    fi

    if /usr/bin/file "$f" | grep -q "Mach-O"; then
      sign_code_file "$f"
    fi
  done < <(find "$dir" -type f -print0)
}

# ===== Build .app =====
if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  BUILD_ARGS=()

  if [[ -n "$TAURI_TARGET" ]]; then
    BUILD_ARGS+=(--target "$TAURI_TARGET")
  fi

  # Build only the .app here. package-macos.sh creates the final signed/notarized
  # DMG itself after adding the privileged helper. This prevents accidentally
  # distributing Tauri's earlier, pre-helper DMG.
  cargo tauri build --bundles app "${BUILD_ARGS[@]}"
fi

# Do not overwrite APP_PATH parsed from --app.
if [[ -z "$APP_PATH" ]]; then
  if [[ -n "$TAURI_TARGET" ]]; then
    APP_SEARCH_DIR="$ROOT/target/$TAURI_TARGET/release/bundle/macos"
  else
    APP_SEARCH_DIR="$ROOT/target/release/bundle/macos"
  fi

  APP_PATH="$(
    find "$APP_SEARCH_DIR" -maxdepth 2 -type d -name '*.app' 2>/dev/null \
      | head -n 1 || true
  )"
fi

if [[ -z "$APP_PATH" || ! -d "$APP_PATH/Contents" ]]; then
  echo "APP_PATH not found or invalid: ${APP_PATH:-<empty>}" >&2
  exit 1
fi

APP_CONTENTS="$APP_PATH/Contents"
INFO_PLIST="$APP_CONTENTS/Info.plist"
LS_DIR="$APP_CONTENTS/Library/LaunchServices"
mkdir -p "$LS_DIR"

APP_BUNDLE_ID="$(get_tauri_identifier)"
if [[ -z "$APP_BUNDLE_ID" ]]; then
  APP_BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$INFO_PLIST" 2>/dev/null || true)"
fi
if [[ -z "$APP_BUNDLE_ID" ]]; then
  echo "Cannot determine app bundle identifier" >&2
  exit 1
fi

# Ensure app Info.plist has correct CFBundleIdentifier
plist_set_string "$INFO_PLIST" ":CFBundleIdentifier" "$APP_BUNDLE_ID"

# ===== Sign inner Mach-O (excluding helper for now) =====
# Do not sign the app's main executable separately. Signing the .app bundle below
# signs the main executable. Nested executables (especially Tauri sidecars) must
# already be signed before the outer bundle is signed.
APP_EXECUTABLE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$INFO_PLIST" 2>/dev/null || true)"
MAIN_EXECUTABLE=""
if [[ -n "$APP_EXECUTABLE" ]]; then
  MAIN_EXECUTABLE="$APP_CONTENTS/MacOS/$APP_EXECUTABLE"
fi

# Validate target architecture before signing. This makes the packaging step
# fail instead of producing a DMG that Finder cannot launch on the target Mac.
if [[ -n "$EXPECTED_ARCHS" && -n "$MAIN_EXECUTABLE" ]]; then
  require_archs "$MAIN_EXECUTABLE" "$EXPECTED_ARCHS"
fi

# Sign sing-box explicitly first. This avoids codesign rejecting the outer app
# because an unsigned sidecar is present under Contents/MacOS.
SING_BOX="$APP_CONTENTS/MacOS/sing-box"
if [[ -n "$EXPECTED_ARCHS" && -f "$SING_BOX" ]]; then
  require_archs "$SING_BOX" "$EXPECTED_ARCHS"
fi
if [[ -f "$SING_BOX" ]]; then
  echo "=== Signing sing-box ==="
  /usr/bin/file "$SING_BOX"
  sign_code_file "$SING_BOX"
fi

sign_macho_under "$APP_CONTENTS/Frameworks"
sign_macho_under "$APP_CONTENTS/Resources"

# Sign any other Mach-O sidecars under Contents/MacOS, but skip the main app
# executable and sing-box (already signed above).
if [[ -d "$APP_CONTENTS/MacOS" ]]; then
  while IFS= read -r -d '' f; do
    if [[ -n "$MAIN_EXECUTABLE" && "$f" == "$MAIN_EXECUTABLE" ]]; then
      continue
    fi
    if [[ "$f" == "$SING_BOX" ]]; then
      continue
    fi
    if /usr/bin/file "$f" | grep -q "Mach-O"; then
      sign_code_file "$f"
    fi
  done < <(find "$APP_CONTENTS/MacOS" -type f -print0)
fi

# Fail early with a clear error if sing-box somehow remained unsigned.
if [[ -f "$SING_BOX" ]]; then
  /usr/bin/codesign --verify --strict --verbose=2 "$SING_BOX"
fi

# First sign the app so we can derive APP_REQ.
/usr/bin/codesign --force --options runtime --timestamp \
  --entitlements "$APP_ENTITLEMENTS" \
  --sign "$SIGN_IDENTITY" \
  "$APP_PATH"

APP_REQ="$(/usr/bin/codesign -dr - "$APP_PATH" 2>&1 | sed -n 's/^designated => //p')"
if [[ -z "$APP_REQ" ]]; then
  echo "App designated requirement is empty" >&2
  exit 1
fi

# ===== Helper paths =====
HELPER_DIR="$ROOT/macos/helper"
HELPER_BUILD="$HELPER_DIR/build-helper.sh"
HELPER_ENTITLEMENTS="$ROOT/$HELPER_ENTITLEMENTS_REL"

# Launchd plist source (in repository)
HELPER_PLIST_SRC=""
if [[ -f "$HELPER_DIR/ru.ravel.ultunnel-macos.helper.plist" ]]; then
  HELPER_PLIST_SRC="$HELPER_DIR/ru.ravel.ultunnel-macos.helper.plist"
elif [[ -f "$HELPER_DIR/UltunnelPrivilegedHelper/Helper/ru.ravel.ultunnel-macos.helper.plist" ]]; then
  HELPER_PLIST_SRC="$HELPER_DIR/UltunnelPrivilegedHelper/Helper/ru.ravel.ultunnel-macos.helper.plist"
else
  echo "Missing helper launchd plist in helper dir" >&2
  exit 1
fi

# Helper Info.plist (embedded into binary as __info_plist)
HELPER_INFO_PLIST_PROJECT="$HELPER_DIR/UltunnelPrivilegedHelper/Helper/Info.plist"

if [[ ! -f "$HELPER_BUILD" ]]; then
  echo "Missing helper build script: $HELPER_BUILD" >&2
  exit 1
fi
if [[ ! -f "$HELPER_PLIST_SRC" ]]; then
  echo "Missing helper launchd plist: $HELPER_PLIST_SRC" >&2
  exit 1
fi
if [[ ! -f "$HELPER_ENTITLEMENTS" ]]; then
  echo "Missing helper entitlements: $HELPER_ENTITLEMENTS" >&2
  exit 1
fi
if [[ ! -f "$HELPER_INFO_PLIST_PROJECT" ]]; then
  echo "Missing helper project Info.plist: $HELPER_INFO_PLIST_PROJECT" >&2
  exit 1
fi

# ===== Patch helper Info.plist + launchd plist TEMPORARILY =====
TMP_HELPER_INFO="$(mktemp)"
TMP_HELPER_LAUNCHD="$(mktemp)"
cp -f "$HELPER_INFO_PLIST_PROJECT" "$TMP_HELPER_INFO"
cp -f "$HELPER_PLIST_SRC" "$TMP_HELPER_LAUNCHD"

restore_helper_plists() {
  mv -f "$TMP_HELPER_INFO" "$HELPER_INFO_PLIST_PROJECT" 2>/dev/null || true
  mv -f "$TMP_HELPER_LAUNCHD" "$HELPER_PLIST_SRC" 2>/dev/null || true
}
trap restore_helper_plists EXIT

# 1) Info.plist: CFBundleIdentifier + SMAuthorizedClients = [APP_REQ]
plist_set_string "$HELPER_INFO_PLIST_PROJECT" ":CFBundleIdentifier" "$HELPER_LABEL"
plist_delete_key "$HELPER_INFO_PLIST_PROJECT" ":SMAuthorizedClients"
plist_set_or_add "$HELPER_INFO_PLIST_PROJECT" ":SMAuthorizedClients" "array" ""
plist_set_string "$HELPER_INFO_PLIST_PROJECT" ":SMAuthorizedClients:0" "$APP_REQ"

# 2) launchd plist: Label + ProgramArguments[0] + MachServices[HELPER_LABEL]=true
plist_set_string "$HELPER_PLIST_SRC" ":Label" "$HELPER_LABEL"

plist_delete_key "$HELPER_PLIST_SRC" ":Program"
plist_delete_key "$HELPER_PLIST_SRC" ":ProgramArguments"
plist_set_or_add "$HELPER_PLIST_SRC" ":ProgramArguments" "array" ""
plist_set_string "$HELPER_PLIST_SRC" ":ProgramArguments:0" "/Library/PrivilegedHelperTools/$HELPER_LABEL"

plist_delete_key "$HELPER_PLIST_SRC" ":MachServices"
plist_set_or_add "$HELPER_PLIST_SRC" ":MachServices" "dict" ""
plist_set_or_add "$HELPER_PLIST_SRC" ":MachServices:$HELPER_LABEL" "bool" "true"

plist_set_or_add "$HELPER_PLIST_SRC" ":RunAtLoad" "bool" "true"

# ===== Build helper (Swift helper; must contain embedded __info_plist and __launchd_plist) =====
HELPER_TMP_DIR="$(mktemp -d -t ultunnel-helper-out.XXXXXX)"
HELPER_BIN="$(
  LABEL="$HELPER_LABEL" \
  ARCH="$HELPER_ARCH" \
  OUT_DIR="$HELPER_TMP_DIR" \
  bash "$HELPER_BUILD" | tail -n 1
)"
if [[ -z "${HELPER_BIN:-}" || ! -f "$HELPER_BIN" ]]; then
  echo "Helper build failed or helper binary not found: ${HELPER_BIN:-<empty>}" >&2
  exit 1
fi
if [[ -n "$EXPECTED_ARCHS" ]]; then
  require_archs "$HELPER_BIN" "$EXPECTED_ARCHS"
fi

# Restore repo plists now (so packaging doesn't leave workspace dirty)
restore_helper_plists
trap - EXIT

# ===== Embed helper into .app =====
cp -f "$HELPER_BIN" "$LS_DIR/$HELPER_LABEL"
cp -f "$HELPER_PLIST_SRC" "$LS_DIR/$HELPER_LABEL.plist"
rm -rf "$HELPER_TMP_DIR"

chmod 755 "$LS_DIR/$HELPER_LABEL" || true
chmod 644 "$LS_DIR/$HELPER_LABEL.plist" || true

# Patch embedded launchd plist too (must match exactly)
plist_set_string "$LS_DIR/$HELPER_LABEL.plist" ":Label" "$HELPER_LABEL"
plist_delete_key "$LS_DIR/$HELPER_LABEL.plist" ":Program"
plist_delete_key "$LS_DIR/$HELPER_LABEL.plist" ":ProgramArguments"
plist_set_or_add "$LS_DIR/$HELPER_LABEL.plist" ":ProgramArguments" "array" ""
plist_set_string "$LS_DIR/$HELPER_LABEL.plist" ":ProgramArguments:0" "/Library/PrivilegedHelperTools/$HELPER_LABEL"
plist_delete_key "$LS_DIR/$HELPER_LABEL.plist" ":MachServices"
plist_set_or_add "$LS_DIR/$HELPER_LABEL.plist" ":MachServices" "dict" ""
plist_set_or_add "$LS_DIR/$HELPER_LABEL.plist" ":MachServices:$HELPER_LABEL" "bool" "true"
plist_set_or_add "$LS_DIR/$HELPER_LABEL.plist" ":RunAtLoad" "bool" "true"

# ===== Sign helper inside bundle =====
/usr/bin/codesign --force --options runtime --timestamp \
  --identifier "$HELPER_LABEL" \
  --entitlements "$HELPER_ENTITLEMENTS" \
  --sign "$SIGN_IDENTITY" \
  "$LS_DIR/$HELPER_LABEL"

HELPER_REQ="$(/usr/bin/codesign -dr - "$LS_DIR/$HELPER_LABEL" 2>&1 | sed -n 's/^designated => //p')"
if [[ -z "$HELPER_REQ" ]]; then
  echo "Helper designated requirement is empty after signing" >&2
  exit 1
fi

# Inject SMPrivilegedExecutables[helper] into app Info.plist using exact helper requirement string
HELPER_REQ_ESC="$(printf '%s' "$HELPER_REQ" | sed 's/\\/\\\\/g; s/"/\\"/g')"
/usr/libexec/PlistBuddy -c "Add :SMPrivilegedExecutables dict" "$INFO_PLIST" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Delete :SMPrivilegedExecutables:$HELPER_LABEL" "$INFO_PLIST" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :SMPrivilegedExecutables:$HELPER_LABEL string \"$HELPER_REQ_ESC\"" "$INFO_PLIST"

# Final sign app (Info.plist changed + helper added)
/usr/bin/codesign --force --options runtime --timestamp \
  --entitlements "$APP_ENTITLEMENTS" \
  --sign "$SIGN_IDENTITY" \
  "$APP_PATH"

# Verify the complete nested code signature. Do not hide Gatekeeper failures.
/usr/bin/codesign --verify --deep --strict --verbose=4 "$APP_PATH"
if [[ "${ALLOW_SPCTL_FAILURE:-0}" == "1" || "$NOTARY" == "1" || -n "$NOTARY_PROFILE" ]]; then
  # Not notarized yet, so Gatekeeper rejects it here; the strict check runs after stapling.
  /usr/sbin/spctl --assess --type execute --verbose=4 "$APP_PATH" || true
else
  /usr/sbin/spctl --assess --type execute --verbose=4 "$APP_PATH"
fi

# Print resulting architectures for quick verification.
echo "=== Architectures ==="
if [[ -n "$APP_EXECUTABLE" && -f "$APP_CONTENTS/MacOS/$APP_EXECUTABLE" ]]; then
  file "$APP_CONTENTS/MacOS/$APP_EXECUTABLE"
fi
file "$LS_DIR/$HELPER_LABEL"
if [[ -f "$APP_CONTENTS/MacOS/sing-box" ]]; then
  file "$APP_CONTENTS/MacOS/sing-box"
fi

echo "OK: app signed: $APP_PATH"

# ===== Build DMG =====
APP_NAME="$(basename "$APP_PATH" .app)"
DMG_VOLNAME="${DMG_VOLNAME:-$APP_NAME}"
if [[ -n "$TAURI_TARGET" ]]; then
  DMG_OUT_DIR="$ROOT/target/$TAURI_TARGET/release/bundle/dmg"
else
  DMG_OUT_DIR="$ROOT/target/release/bundle/dmg"
fi
mkdir -p "$DMG_OUT_DIR"
DMG_OUT="${DMG_OUT:-$DMG_OUT_DIR/${APP_NAME}${DMG_ARCH_SUFFIX}.dmg}"

STAGE="$(mktemp -d)"
mkdir -p "$STAGE/$DMG_VOLNAME"
ditto "$APP_PATH" "$STAGE/$DMG_VOLNAME/$APP_NAME.app"
ln -s /Applications "$STAGE/$DMG_VOLNAME/Applications"

hdiutil create \
  -volname "$DMG_VOLNAME" \
  -srcfolder "$STAGE/$DMG_VOLNAME" \
  -ov -format UDZO \
  "$DMG_OUT"

rm -rf "$STAGE"

# Sign DMG
/usr/bin/codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG_OUT" || true
/usr/bin/codesign --verify --verbose=2 "$DMG_OUT" || true

echo "OK: dmg built (+ signed): $DMG_OUT"

# ===== Optional notarization =====
if [[ "$NOTARY" == "1" || -n "$NOTARY_PROFILE" ]]; then
  if [[ -z "$NOTARY_PROFILE" ]]; then
    echo "NOTARY_PROFILE is empty, skipping notarization" >&2
  else
    echo "Notarizing: $DMG_OUT"
    xcrun notarytool submit "$DMG_OUT" --wait --keychain-profile "$NOTARY_PROFILE"

    # The notary submission includes the app nested in the DMG. Staple the app
    # as well so it can be validated after being copied out of the DMG/offline.
    xcrun stapler staple "$APP_PATH"
    xcrun stapler validate "$APP_PATH"
    xcrun stapler staple "$DMG_OUT"
    xcrun stapler validate "$DMG_OUT"

    # Final checks on exactly the artifact we are about to distribute.
    /usr/bin/codesign --verify --deep --strict --verbose=4 "$APP_PATH"
    /usr/sbin/spctl --assess --type execute --verbose=4 "$APP_PATH"
    echo "OK: app + dmg notarized and stapled: $DMG_OUT"
  fi
fi
