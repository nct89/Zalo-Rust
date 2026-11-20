#!/usr/bin/env bash
# Stage minimal runtime fonts (~1.4 MiB) and call ringtones (~72 KiB) for packages.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DST_DIR="${1:?usage: stage-runtime-assets.sh <dest-zalo-rust-dir>}"
FONTS_SRC="$ROOT/assets/fonts"
SOUNDS_SRC="$ROOT/assets/sounds"

mkdir -p "$DST_DIR/fonts" "$DST_DIR/zcall-sound"

for f in \
  "Zalo-Segoe-UI.dc1a7d868a0b3a0e2daa8b36567e9ea9.ttf" \
  "Zalo-Segoe-UI-Semibold.8de26695631b05cf79e17378f6d1f134.ttf" \
  "Zalo-Segoe-UI-Bold.18b859523696ff3330adac79b99ef554.ttf" \
  "Zalo-Segoe-UI-Text.41364bc2c9bcbffced63c4bad472bfa9.ttf" \
  "zalo-font.6835d695d84d61b03bec2efaf8b51c42.ttf"
do
  [ -f "$FONTS_SRC/$f" ] && install -Dm644 "$FONTS_SRC/$f" "$DST_DIR/fonts/$f"
done

for s in zalo_ringtone.mp3 zalo_ringback.mp3 endcall.mp3; do
  [ -f "$SOUNDS_SRC/$s" ] && install -Dm644 "$SOUNDS_SRC/$s" "$DST_DIR/zcall-sound/$s"
done
