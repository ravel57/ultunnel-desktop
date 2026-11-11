#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
desktop_dir="$(cd "$script_dir/.." && pwd)"
core_dir="${SING_BOX_SOURCE:-$desktop_dir/../../GolandProjects/sing-box}"
[[ -f "$core_dir/go.mod" ]] || { echo "Set SING_BOX_SOURCE to the sing-box fork directory" >&2; exit 1; }
triple="$(rustc -vV | sed -n 's/^host: //p')"
[[ -n "$triple" ]] || { echo "Cannot detect Rust target triple" >&2; exit 1; }
tags="$(tr -d '\r\n' < "$core_dir/release/DEFAULT_BUILD_TAGS_OTHERS"),with_naive_outbound"
mkdir -p "$desktop_dir/src-tauri/bin"
cd "$core_dir"
go build -tags "$tags" -o "$desktop_dir/src-tauri/bin/sing-box-$triple" ./cmd/sing-box
printf 'Core with Naive support: %s\nRebuild the desktop app to bundle it.\n' "$desktop_dir/src-tauri/bin/sing-box-$triple"
