#!/usr/bin/env bash
set -euo pipefail

pad4() {
  local f="$1"
  python3 - "$f" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1])
b = p.read_bytes()
pad = (-len(b)) % 4
if pad:
    p.write_bytes(b + b"\0"*pad)
PY
}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Ищем helper-проект рядом со скриптом
if [[ -d "$SCRIPT_DIR/UltunnelPrivilegedHelper/UltunnelPrivilegedHelper.xcodeproj" ]]; then
  HELPER_ROOT="$SCRIPT_DIR"
elif [[ -d "$SCRIPT_DIR/../helper/UltunnelPrivilegedHelper/UltunnelPrivilegedHelper.xcodeproj" ]]; then
  HELPER_ROOT="$(cd "$SCRIPT_DIR/../helper" && pwd)"
else
  echo "Cannot find UltunnelPrivilegedHelper.xcodeproj near: $SCRIPT_DIR" >&2
  exit 1
fi

MACOS_DIR="$(cd "$HELPER_ROOT/.." && pwd)"
DERIVED="$MACOS_DIR/.deriveddata"

CONFIG="${CONFIG:-Release}"
SCHEME="${SCHEME:-UltunnelPrivilegedHelper}"
PROJECT="$HELPER_ROOT/UltunnelPrivilegedHelper/UltunnelPrivilegedHelper.xcodeproj"

# Исходные plist'ы
INFO_PLIST_SRC="$HELPER_ROOT/UltunnelPrivilegedHelper/Helper/Info.plist"

LAUNCHD_PLIST_SRC=""
if [[ -f "$HELPER_ROOT/ru.ravel.ultunnel-macos.helper.plist" ]]; then
  LAUNCHD_PLIST_SRC="$HELPER_ROOT/ru.ravel.ultunnel-macos.helper.plist"
elif [[ -f "$HELPER_ROOT/UltunnelPrivilegedHelper/Helper/ru.ravel.ultunnel-macos.helper.plist" ]]; then
  LAUNCHD_PLIST_SRC="$HELPER_ROOT/UltunnelPrivilegedHelper/Helper/ru.ravel.ultunnel-macos.helper.plist"
else
  echo "Cannot find ru.ravel.ultunnel-macos.helper.plist in:" >&2
  echo " - $HELPER_ROOT/ru.ravel.ultunnel-macos.helper.plist" >&2
  echo " - $HELPER_ROOT/UltunnelPrivilegedHelper/Helper/ru.ravel.ultunnel-macos.helper.plist" >&2
  exit 1
fi

[[ -s "$INFO_PLIST_SRC" ]] || { echo "Missing/empty: $INFO_PLIST_SRC" >&2; exit 1; }
[[ -s "$LAUNCHD_PLIST_SRC" ]] || { echo "Missing/empty: $LAUNCHD_PLIST_SRC" >&2; exit 1; }

# Работаем с временными копиями, чтобы:
# - pad4 не менял файлы в репозитории
# - именно эти (падденные) файлы вшивались в бинарь
TMP_INFO="$(mktemp -t helper.info.XXXXXX.plist)"
TMP_LAUNCHD="$(mktemp -t helper.launchd.XXXXXX.plist)"
cleanup() { rm -f "$TMP_INFO" "$TMP_LAUNCHD"; }
trap cleanup EXIT

cp -f "$INFO_PLIST_SRC" "$TMP_INFO"
cp -f "$LAUNCHD_PLIST_SRC" "$TMP_LAUNCHD"

# Проверяем валидность исходников (до паддинга)
plutil -lint "$TMP_INFO"
plutil -lint "$TMP_LAUNCHD"

# Делаем длину кратной 4 (некоторые связки otool/sectcreate/парсеры чувствительны)
pad4 "$TMP_INFO"
pad4 "$TMP_LAUNCHD"

# Ещё раз проверим после паддинга
plutil -lint "$TMP_INFO"
plutil -lint "$TMP_LAUNCHD"

# Вшиваем plist'ы в бинарь helper при линковке
OTHER_LDFLAGS_VALUE="-sectcreate __TEXT __info_plist \"$TMP_INFO\" -sectcreate __TEXT __launchd_plist \"$TMP_LAUNCHD\""

XCB_ARGS=(
  -project "$PROJECT"
  -scheme "$SCHEME"
  -configuration "$CONFIG"
  -derivedDataPath "$DERIVED"
  OTHER_LDFLAGS="$OTHER_LDFLAGS_VALUE"
  build
)

if [[ -n "${SIGN_IDENTITY:-}" ]]; then
  XCB_ARGS+=( CODE_SIGN_IDENTITY="$SIGN_IDENTITY" )
fi
if [[ -n "${TEAM_ID:-}" ]]; then
  XCB_ARGS+=( DEVELOPMENT_TEAM="$TEAM_ID" CODE_SIGN_STYLE=Manual )
fi

xcodebuild "${XCB_ARGS[@]}" 1>&2

PRODUCTS_DIR="$DERIVED/Build/Products/$CONFIG"

# Где лежит бинарь: сначала как раньше ($SCHEME), если нет — найдём исполняемый файл в PRODUCTS_DIR
BIN="$PRODUCTS_DIR/$SCHEME"
if [[ ! -f "$BIN" ]]; then
  BIN="$(find "$PRODUCTS_DIR" -maxdepth 1 -type f -perm -111 2>/dev/null | head -n 1 || true)"
fi

if [[ -z "${BIN:-}" || ! -f "$BIN" ]]; then
  echo "Helper not found in: $PRODUCTS_DIR" >&2
  ls -la "$PRODUCTS_DIR" >&2 || true
  exit 1
fi

# Жёсткая проверка: секции должны реально присутствовать
if ! otool -l "$BIN" | grep -q "__launchd_plist"; then
  echo "__launchd_plist is NOT embedded into helper binary: $BIN" >&2
  exit 1
fi
if ! otool -l "$BIN" | grep -q "__info_plist"; then
  echo "__info_plist is NOT embedded into helper binary: $BIN" >&2
  exit 1
fi

echo "$BIN"
