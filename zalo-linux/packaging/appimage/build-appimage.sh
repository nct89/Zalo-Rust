#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml" | python3 -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
APPDIR="$TARGET_DIR/Zalo.AppDir"
OUT_APPIMAGE="${OUT_APPIMAGE:-$ROOT/dist/Zalo-Linux-Rust-x86_64.AppImage}"

cargo build --release -p zalo-gpui --features tray-xembed --manifest-path "$ROOT/Cargo.toml"

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" \
  "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/icons/hicolor/256x256/apps"

install -Dm755 "$TARGET_DIR/release/zalo-gpui" "$APPDIR/usr/bin/zalo-gpui"
strip --strip-all "$APPDIR/usr/bin/zalo-gpui" || true
bash "$ROOT/packaging/linux/stage-runtime-assets.sh" "$APPDIR/usr/bin"
cp "$ROOT/packaging/linux/zalo-gpui.desktop" "$APPDIR/zalo-gpui.desktop"
sed -i 's|^Exec=/usr/bin/zalo-gpui|Exec=zalo-gpui|g' "$APPDIR/zalo-gpui.desktop"
cp "$APPDIR/zalo-gpui.desktop" "$APPDIR/usr/share/applications/zalo-gpui.desktop"
install -Dm644 "$ROOT/crates/zalo-shell/assets/zalo-256.png" \
  "$APPDIR/zalo-gpui.png"
install -Dm644 "$ROOT/crates/zalo-shell/assets/zalo-256.png" \
  "$APPDIR/usr/share/icons/hicolor/256x256/apps/zalo-gpui.png"

cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export ZALO_DESKTOP_FILE="${ZALO_DESKTOP_FILE:-zalo-gpui.desktop}"
exec "$HERE/usr/bin/zalo-gpui" "$@"
EOF
chmod 755 "$APPDIR/AppRun"

if command -v appimagetool >/dev/null 2>&1; then
  mkdir -p "$ROOT/dist"
  ARCH="${ARCH:-x86_64}" appimagetool "$APPDIR" "$OUT_APPIMAGE"
  echo "Done: $OUT_APPIMAGE"
else
  echo "Staged AppDir at $APPDIR (install appimagetool to pack $OUT_APPIMAGE)"
fi
