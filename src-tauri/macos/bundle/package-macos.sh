#!/usr/bin/env bash
set -euo pipefail

# ====== НАСТРОЙКИ ======
HELPER_LABEL="ru.ravel.ultunnel-macos.helper"
SIGN_IDENTITY="Developer ID Application: Petr Lomakin (ASMHMRKL3K)"   # Developer ID Application: ...
# =======================

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"   # src-tauri/
DERIVED="$ROOT/macos/.deriveddata"

HELPER_PROJECT="$ROOT/macos/helper/UltunnelPrivilegedHelper.xcodeproj"
HELPER_SCHEME="UltunnelPrivilegedHelper"

INSTALLER_PROJECT="$ROOT/macos/installer/UltunnelHelperInstaller.xcodeproj"
INSTALLER_SCHEME="UltunnelHelperInstaller"

APP_DIR="$ROOT/target/release/bundle/macos"
APP_PATH="${1:-}"

# Если передали .../Something.app/Contents — поднимем на .app
if [[ -n "$APP_PATH" && "$(basename "$APP_PATH")" == "Contents" ]]; then
  APP_PATH="$(cd "$APP_PATH/.." && pwd)"
fi

# Если путь не передали — попробуем найти .app автоматически
if [[ -z "$APP_PATH" ]]; then
  if compgen -G "$APP_DIR"/*.app > /dev/null; then
    APP_PATH="$(ls -1 "$APP_DIR"/*.app | head -n 1)"
  else
    APP_PATH="$(find "$ROOT/target" -maxdepth 8 -type d -name "*.app" | head -n 1)"
  fi
fi

if [[ -z "$APP_PATH" || ! -d "$APP_PATH" ]]; then
  echo "APP not found: ${APP_PATH:-<empty>}" >&2
  echo "Try: find src-tauri/target -name '*.app' -maxdepth 8 -type d" >&2
  exit 1
fi

[[ -n "$SIGN_IDENTITY" ]] || { echo "Set SIGN_IDENTITY env var" >&2; exit 1; }
[[ -d "$HELPER_PROJECT" ]] || { echo "Helper project not found: $HELPER_PROJECT" >&2; exit 1; }
[[ -d "$INSTALLER_PROJECT" ]] || { echo "Installer project not found: $INSTALLER_PROJECT" >&2; exit 1; }

# 1) Собираем helper (release) в предсказуемый DerivedData
xcodebuild \
  -project "$HELPER_PROJECT" \
  -scheme "$HELPER_SCHEME" \
  -configuration Release \
  -derivedDataPath "$DERIVED" \
  build

HELPER_BIN="$DERIVED/Build/Products/Release/$HELPER_SCHEME"

# 2) Собираем installer (release)
xcodebuild \
  -project "$INSTALLER_PROJECT" \
  -scheme "$INSTALLER_SCHEME" \
  -configuration Release \
  -derivedDataPath "$DERIVED" \
  build

INSTALLER_BIN="$DERIVED/Build/Products/Release/$INSTALLER_SCHEME"

# 3) Кладём файлы внутрь .app в ожидаемые места
APP_CONTENTS="$APP_PATH/Contents"
LS_DIR="$APP_CONTENTS/Library/LaunchServices"
RES_DIR="$APP_CONTENTS/Resources"
INFO_PLIST="$APP_CONTENTS/Info.plist"

mkdir -p "$LS_DIR" "$RES_DIR"

cp -f "$HELPER_BIN" "$LS_DIR/$HELPER_LABEL"
cp -f "$ROOT/macos/helper/Helper/$HELPER_LABEL.plist" "$LS_DIR/$HELPER_LABEL.plist"
cp -f "$INSTALLER_BIN" "$RES_DIR/ultunnel-helper-installer"

chmod 755 "$LS_DIR/$HELPER_LABEL" "$RES_DIR/ultunnel-helper-installer"

# 4) Подписываем вложенные бинарники (до requirement)

# Helper ОБЯЗАТЕЛЬНО подписываем с identifier == HELPER_LABEL (это нужно SMJobBless)
codesign --force --options runtime --timestamp \
  --identifier "$HELPER_LABEL" \
  --sign "$SIGN_IDENTITY" \
  "$LS_DIR/$HELPER_LABEL"

# Installer можно без identifier
codesign --force --options runtime --timestamp \
  --sign "$SIGN_IDENTITY" \
  "$RES_DIR/ultunnel-helper-installer"

# Подписываем все Mach-O в Contents/MacOS (main + sidecars типа sing-box)
APP_MACOS_DIR="$APP_CONTENTS/MacOS"
if [[ -d "$APP_MACOS_DIR" ]]; then
  while IFS= read -r -d '' f; do
    if file "$f" | grep -q "Mach-O"; then
      codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$f"
    fi
  done < <(find "$APP_MACOS_DIR" -maxdepth 1 -type f -print0)
fi

# 5) Получаем designated requirement helper'а
HELPER_REQ="$(codesign -dr - "$LS_DIR/$HELPER_LABEL" 2>&1 | sed -n 's/^designated => //p')"
[[ -n "$HELPER_REQ" ]] || { echo "Helper designated requirement empty (signing?)" >&2; exit 1; }

# 6) Инжектим SMPrivilegedExecutables в Info.plist приложения
/usr/libexec/PlistBuddy -c "Add :SMPrivilegedExecutables dict" "$INFO_PLIST" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Delete :SMPrivilegedExecutables:$HELPER_LABEL" "$INFO_PLIST" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :SMPrivilegedExecutables:$HELPER_LABEL string $HELPER_REQ" "$INFO_PLIST"

# 7) Подписываем весь .app после изменений Info.plist
codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP_PATH"

echo "OK: prepared and signed app: $APP_PATH"

# ====== DMG ======
# Можно переопределить:
#   DMG_OUT="/path/to/file.dmg"
#   DMG_VOLNAME="ultunnel-desktop"
#   NOTARY_PROFILE="ULTUNNEL_NOTARY"   (если хочешь сразу notarize+staple)
#   NOTARY=1                           (включить notarize даже если профиль задан)
APP_NAME="$(basename "$APP_PATH" .app)"
DMG_VOLNAME="${DMG_VOLNAME:-$APP_NAME}"
DMG_OUT_DIR="$ROOT/target/release/bundle/dmg"
mkdir -p "$DMG_OUT_DIR"
DMG_OUT="${DMG_OUT:-$DMG_OUT_DIR/${APP_NAME}.dmg}"

STAGE="$(mktemp -d)"
mkdir -p "$STAGE/$DMG_VOLNAME"

# ВАЖНО: копируем .app через ditto, чтобы не ломать sealed resources
ditto "$APP_PATH" "$STAGE/$DMG_VOLNAME/$APP_NAME.app"
ln -s /Applications "$STAGE/$DMG_VOLNAME/Applications"

hdiutil create \
  -volname "$DMG_VOLNAME" \
  -srcfolder "$STAGE/$DMG_VOLNAME" \
  -ov -format UDZO \
  "$DMG_OUT"

rm -rf "$STAGE"

# Подпись DMG (не обязательна для notarization, но ты просил)
# Если timestamp/сеть подведут — лучше чтобы скрипт не падал из-за подписи DMG
codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG_OUT" || true

echo "OK: dmg built: $DMG_OUT"

# ====== OPTIONAL: notarize + staple ======
# Если хочешь автоматом: export NOTARY_PROFILE="ULTUNNEL_NOTARY" и (опц.) NOTARY=1
if [[ "${NOTARY:-0}" == "1" || -n "${NOTARY_PROFILE:-}" ]]; then
  if [[ -z "${NOTARY_PROFILE:-}" ]]; then
    echo "NOTARY_PROFILE is empty, skipping notarization" >&2
  else
    echo "Notarizing: $DMG_OUT"
    xcrun notarytool submit "$DMG_OUT" --wait --keychain-profile "$NOTARY_PROFILE"
    xcrun stapler staple "$DMG_OUT"
    xcrun stapler validate "$DMG_OUT"
    echo "OK: dmg notarized+stapled: $DMG_OUT"
  fi
fi
