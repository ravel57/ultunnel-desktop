#!/usr/bin/env bash
set -euo pipefail

SRC_TAURI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# 1) Где лежит собранный .app после tauri build
APP_DIR_DEFAULT="$SRC_TAURI_DIR/target/release/bundle/macos"
APP_PATH="${1:-}"

if [[ -z "${APP_PATH}" ]]; then
  APP_PATH="$(ls -1 "$APP_DIR_DEFAULT"/*.app | head -n 1)"
fi

if [[ ! -d "$APP_PATH" ]]; then
  echo "APP not found: $APP_PATH" >&2
  exit 1
fi

# 2) Сборка helper/installer (можно убрать, если собираете отдельно)
HELPER_BIN="$(bash "$SRC_TAURI_DIR/macos/helper/build-helper.sh")"
INSTALLER_BIN="$(bash "$SRC_TAURI_DIR/macos/installer/build-installer.sh")"

# 3) Пути внутри .app
APP_CONTENTS="$APP_PATH/Contents"
LS_DIR="$APP_CONTENTS/Library/LaunchServices"
RES_DIR="$APP_CONTENTS/Resources"
INFO_PLIST="$APP_CONTENTS/Info.plist"

HELPER_LABEL="ru.ravel.ultunnel-macos.helper"

mkdir -p "$LS_DIR" "$RES_DIR"

# helper executable должен лежать здесь:
# Contents/Library/LaunchServices/ru.ravel.ultunnel-macos.helper
cp -f "$HELPER_BIN" "$LS_DIR/$HELPER_LABEL"

# launchd plist должен лежать рядом
cp -f "$SRC_TAURI_DIR/macos/helper/Helper/$HELPER_LABEL.plist" "$LS_DIR/$HELPER_LABEL.plist"

# installer кладём в Resources
cp -f "$INSTALLER_BIN" "$RES_DIR/ultunnel-helper-installer"

chmod 755 "$LS_DIR/$HELPER_LABEL" "$RES_DIR/ultunnel-helper-installer"

# 4) Добавляем SMPrivilegedExecutables в Info.plist приложения
# Получаем designated requirement helper (после подписи это будет точнее, но для старта годится)
HELPER_REQ="$(/usr/bin/codesign -dr - "$LS_DIR/$HELPER_LABEL" 2>&1 | sed -n 's/^designated => //p')"

if [[ -z "$HELPER_REQ" ]]; then
  echo "Helper designated requirement is empty. Helper должен быть подписан." >&2
  echo "Подпишите helper в Xcode (Signing & Capabilities) и повторите." >&2
  exit 1
fi

/usr/libexec/PlistBuddy -c "Add :SMPrivilegedExecutables dict" "$INFO_PLIST" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Delete :SMPrivilegedExecutables:$HELPER_LABEL" "$INFO_PLIST" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :SMPrivilegedExecutables:$HELPER_LABEL string $HELPER_REQ" "$INFO_PLIST"

echo "Injected SMPrivilegedExecutables[$HELPER_LABEL]"

# 5) Codesign (нужно указать идентичность подписи)
# Пример identity: "Developer ID Application: Your Name (TEAMID)"
SIGN_IDENTITY="${SIGN_IDENTITY:-}"

if [[ -z "$SIGN_IDENTITY" ]]; then
  echo "Set SIGN_IDENTITY env var, example:"
  echo '  SIGN_IDENTITY="Developer ID Application: NAME (TEAMID)" bash copy-privileged.sh'
  exit 1
fi

# Подписываем вложенные бинарники
/usr/bin/codesign --force --options runtime --timestamp --identifier "$HELPER_LABEL" --sign "$SIGN_IDENTITY" "$LS_DIR/$HELPER_LABEL"
/usr/bin/codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$RES_DIR/ultunnel-helper-installer"

# Подписываем весь .app
/usr/bin/codesign --force --options runtime --timestamp --deep --sign "$SIGN_IDENTITY" "$APP_PATH"

echo "OK: signed $APP_PATH"
echo "App: $APP_PATH"
