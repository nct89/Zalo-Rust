#!/usr/bin/env bash
# Stage runtime fonts (~1.5 MiB), call ringtones (~76 KiB), Sync/Backup WASM (~420 KiB),
# single Zalo emoji sprite pack (~1.7 MiB, 3 files), and UI images (~420 KiB) for packages.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
DST_DIR="${1:?usage: stage-runtime-assets.sh <dest-zalo-rust-dir>}"
FONTS_SRC="$ROOT/assets/fonts"
SOUNDS_SRC="$ROOT/assets/sounds"
WASM_SRC="$ROOT/assets/wasm"
EMOJI_SRC="$ROOT/assets/emoji"
IMAGES_SRC="$ROOT/assets/images"

mkdir -p "$DST_DIR/fonts" "$DST_DIR/zcall-sound" "$DST_DIR/wasm" "$DST_DIR/emoji" "$DST_DIR/images"

for f in \
  "Zalo-Segoe-UI.dc1a7d868a0b3a0e2daa8b36567e9ea9.ttf" \
  "Zalo-Segoe-UI-Semibold.8de26695631b05cf79e17378f6d1f134.ttf" \
  "Zalo-Segoe-UI-Bold.18b859523696ff3330adac79b99ef554.ttf" \
  "Zalo-Segoe-UI-Text.41364bc2c9bcbffced63c4bad472bfa9.ttf" \
  "zalo-font.6835d695d84d61b03bec2efaf8b51c42.ttf"
do
  [ -f "$FONTS_SRC/$f" ] && install -m 644 "$FONTS_SRC/$f" "$DST_DIR/fonts/$f"
done

for s in zalo_ringtone.mp3 zalo_ringback.mp3 endcall.mp3; do
  [ -f "$SOUNDS_SRC/$s" ] && install -m 644 "$SOUNDS_SRC/$s" "$DST_DIR/zcall-sound/$s"
done

for w in \
  "libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm" \
  "libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm"
do
  install -m 644 "$WASM_SRC/$w" "$DST_DIR/wasm/$w"
done

for e in \
  "emoji-pos.json" \
  "emoji-rc.json" \
  "emoji.1e7786c93c8a0c1773f165e2de2fd129.png"
do
  [ -f "$EMOJI_SRC/$e" ] && install -m 644 "$EMOJI_SRC/$e" "$DST_DIR/emoji/$e"
done

if [ -d "$IMAGES_SRC" ]; then
  cp -a "$IMAGES_SRC/." "$DST_DIR/images/"
  chmod -R a+rX "$DST_DIR/images"
fi
