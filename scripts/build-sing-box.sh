#!/usr/bin/env bash
# Usage: build-sing-box.sh [rust-target-triple]   (default: rustc host triple)
# e.g.   build-sing-box.sh x86_64-apple-darwin
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
desktop_dir="$(cd "$script_dir/.." && pwd)"
core_dir="${SING_BOX_SOURCE:-$desktop_dir/../../GolandProjects/sing-box}"
[[ -f "$core_dir/go.mod" ]] || { echo "Set SING_BOX_SOURCE to the sing-box fork directory" >&2; exit 1; }
triple="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
[[ -n "$triple" ]] || { echo "Cannot detect Rust target triple" >&2; exit 1; }
# Имя файла берётся из triple, поэтому GOOS/GOARCH задаём явно — иначе Go собирает
# под свою архитектуру (или под GOARCH из окружения) и бинарь не совпадает с именем.
case "$triple" in
	aarch64-apple-darwin)      goos=darwin;  goarch=arm64; ext="" ;;
	x86_64-apple-darwin)       goos=darwin;  goarch=amd64; ext="" ;;
	x86_64-pc-windows-msvc)    goos=windows; goarch=amd64; ext=".exe" ;;
	aarch64-pc-windows-msvc)   goos=windows; goarch=arm64; ext=".exe" ;;
	x86_64-unknown-linux-gnu)  goos=linux;   goarch=amd64; ext="" ;;
	aarch64-unknown-linux-gnu) goos=linux;   goarch=arm64; ext="" ;;
	*) echo "Unsupported target triple: $triple" >&2; exit 1 ;;
esac
# На Windows naive (cronet) работает только через purego + libcronet.dll — эти теги
# (with_purego, with_naive_outbound) уже есть в DEFAULT_BUILD_TAGS_WINDOWS.
if [[ "$goos" == windows ]]; then
	tags="$(tr -d '\r\n' < "$core_dir/release/DEFAULT_BUILD_TAGS_WINDOWS")"
else
	tags="$(tr -d '\r\n' < "$core_dir/release/DEFAULT_BUILD_TAGS_OTHERS"),with_naive_outbound"
fi
out="$desktop_dir/src-tauri/bin/sing-box-$triple$ext"
mkdir -p "$desktop_dir/src-tauri/bin"
cd "$core_dir"
# Версия из git-тега форка (как в его Makefile), иначе `sing-box version` показывает "unknown".
version="$(CGO_ENABLED=0 GOOS= GOARCH= go run ./cmd/internal/read_tag)"
# cronet (naive outbound) на darwin линкуется через cgo; при кросс-сборке Go по умолчанию
# выключает cgo, поэтому включаем его явно и направляем clang на нужную архитектуру.
if [[ "$goos" == darwin ]]; then
	clang_arch="$([[ "$goarch" == amd64 ]] && echo x86_64 || echo arm64)"
	export CGO_ENABLED=1 CC="xcrun clang -arch $clang_arch" CXX="xcrun clang++ -arch $clang_arch"
	# Без этого clang ставит minos = версия SDK хоста (27.x), и бинарь не запускается
	# на более старых macOS (в т.ч. на любом Intel Mac). 12.0 — минимум для Go 1.25.
	export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
	export CGO_CFLAGS="${CGO_CFLAGS:-} -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET"
	export CGO_LDFLAGS="${CGO_LDFLAGS:-} -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET"
fi
# Общие флаги линковки форка, как в его Makefile (-checklinkname=0 нужен tfo-go на Windows).
ldflags_shared="$(tr -d '\r\n' < "$core_dir/release/LDFLAGS" 2>/dev/null || true)"
GOOS="$goos" GOARCH="$goarch" go build -tags "$tags" \
	-ldflags "-X 'github.com/sagernet/sing-box/constant.Version=$version' $ldflags_shared" \
	-o "$out" ./cmd/sing-box
if [[ "$goos" == darwin ]] && command -v lipo >/dev/null; then
	actual="$(lipo -archs "$out")"
	expected="$([[ "$goarch" == amd64 ]] && echo x86_64 || echo arm64)"
	[[ "$actual" == "$expected" ]] || { echo "Built $out is $actual, expected $expected" >&2; exit 1; }
fi
printf 'Core %s (%s/%s) with Naive support: %s\nRebuild the desktop app to bundle it.\n' "$version" "$goos" "$goarch" "$out"
