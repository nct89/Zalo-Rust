#!/usr/bin/env bash
# Build Zalo.app (and a zip) from a release `zalo-gpui` binary. Run on macOS.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="${VERSION:-0.1.0}"
ARCH="$(uname -m)"
case "$ARCH" in
  arm64) RUST_TARGET="aarch64-apple-darwin" ;;
  x86_64) RUST_TARGET="x86_64-apple-darwin" ;;
  *) echo "unsupported arch: $ARCH" >&2; exit 1 ;;
esac

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "build-app.sh must run on macOS (current: $(uname -s))" >&2
  exit 1
fi

cd "$ROOT"
# CMake 4+ (Homebrew on macOS) rejects older cmake_minimum_required(VERSION 3.1)
# in bundled audiopus_sys unless compatibility floor is raised to 3.5.
export CMAKE_POLICY_VERSION_MINIMUM="${CMAKE_POLICY_VERSION_MINIMUM:-3.5}"
cargo build --release -p zalo-gpui --features tray-xembed

STAGE="$ROOT/dist/Zalo.app"
rm -rf "$STAGE"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources/zalo-rust"

install -m 755 "$ROOT/target/release/zalo-gpui" "$STAGE/Contents/MacOS/zalo-gpui"
sed "s/0\\.1\\.0/${VERSION}/g" "$ROOT/zalo-macos/packaging/Info.plist" > "$STAGE/Contents/Info.plist"
install -m 644 "$ROOT/protocol.json" "$STAGE/Contents/Resources/zalo-rust/protocol.json"
bash "$ROOT/zalo-linux/packaging/linux/stage-runtime-assets.sh" "$STAGE/Contents/Resources/zalo-rust"

ICON_SRC="$ROOT/zalo-linux/crates/zalo-shell/assets/zalo-256.png"
ICONSET="$ROOT/dist/AppIcon.iconset"
if command -v sips >/dev/null && command -v iconutil >/dev/null && [[ -f "$ICON_SRC" ]]; then
  rm -rf "$ICONSET"
  mkdir -p "$ICONSET"
  sips -z 256 256 "$ICON_SRC" --out "$ICONSET/icon_256x256.png" >/dev/null
  sips -z 512 512 "$ICON_SRC" --out "$ICONSET/icon_512x512.png" >/dev/null
  iconutil -c icns "$ICONSET" -o "$STAGE/Contents/Resources/AppIcon.icns"
  rm -rf "$ICONSET"
fi

# Ad-hoc signature so Apple Silicon will launch a local build. Notarization
# needs an Apple Developer ID and is intentionally not done here.
codesign --force --deep --sign - "$STAGE"

ZIP="$ROOT/dist/Zalo-${VERSION}-macos-${RUST_TARGET}.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$STAGE" "$ZIP"
echo "wrote $STAGE"
echo "wrote $ZIP"
