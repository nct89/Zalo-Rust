#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
LINUX_DIR="$ROOT/zalo-linux"
WORKSPACE_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)"
VERSION="${VERSION:-${WORKSPACE_VERSION:-0.3.3}}"
export VERSION
export ZALO_BUILD_VERSION="${ZALO_BUILD_VERSION:-$VERSION}"
ARCH="${ARCH:-$(dpkg --print-architecture 2>/dev/null || echo amd64)}"
TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml" | python3 -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
STAGE="$TARGET_DIR/deb-pkg"
OUT_DEB="$ROOT/dist/zalo-linux_${VERSION}_${ARCH}.deb"

echo "[1/3] Building release binary (SNI + XEmbed fallback)..."
cargo build --release -p zalo-gpui --features tray-xembed --manifest-path "$ROOT/Cargo.toml"

echo "[2/3] Staging Debian package tree..."
rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" \
  "$STAGE/usr/bin" \
  "$STAGE/usr/share/applications" \
  "$STAGE/usr/share/icons/hicolor/256x256/apps" \
  "$STAGE/usr/share/pixmaps"

install -Dm755 "$TARGET_DIR/release/zalo-gpui" "$STAGE/usr/bin/zalo-gpui"
strip --strip-all "$STAGE/usr/bin/zalo-gpui" || true
bash "$LINUX_DIR/packaging/linux/stage-runtime-assets.sh" "$STAGE/usr/share/zalo-rust"
install -Dm644 "$LINUX_DIR/packaging/linux/zalo-gpui.desktop" "$STAGE/usr/share/applications/zalo-gpui.desktop"
install -Dm644 "$LINUX_DIR/crates/zalo-shell/assets/zalo-256.png" \
  "$STAGE/usr/share/icons/hicolor/256x256/apps/zalo-gpui.png"
install -Dm644 "$LINUX_DIR/crates/zalo-shell/assets/zalo-256.png" \
  "$STAGE/usr/share/pixmaps/zalo-gpui.png"

cat > "$STAGE/DEBIAN/control" <<EOF
Package: zalo-linux
Version: ${VERSION}
Section: net
Priority: optional
Architecture: ${ARCH}
Maintainer: Zalo-Rust contributors
Depends: libc6, libwayland-client0 | libx11-6
Recommends: gnome-shell-extension-appindicator | gir1.2-ayatanaappindicator3-0.1, libnotify4, xdg-desktop-portal
Description: Native Rust Zalo Desktop client (GPUI)
 Native Linux Zalo client built in Rust with StatusNotifierItem + DBusMenu,
 X11 XEmbed tray fallback, Dock Quicklist actions, and XDG Portal support.
EOF

cat > "$STAGE/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -f -t /usr/share/icons/hicolor || true
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications || true
EOF
chmod 755 "$STAGE/DEBIAN/postinst"

echo "[3/3] Packing .deb (-Zxz) -> $OUT_DEB"
mkdir -p "$ROOT/dist"
dpkg-deb -Zxz --root-owner-group --build "$STAGE" "$OUT_DEB"
echo "Done: $OUT_DEB"
