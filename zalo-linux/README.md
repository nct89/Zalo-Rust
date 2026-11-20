# Zalo-Rust for Linux (`zalo-linux`)

Native Linux desktop client for Zalo built in Rust with [GPUI](https://www.gpui.rs/), supporting both **Wayland** and **X11** across major Linux distributions (Debian, Ubuntu, Linux Mint, Fedora, openSUSE, Arch Linux).

## Features

- **Native GPU-accelerated UI (GPUI)** with Light/Dark/System themes, English/Vietnamese UI toggle, and distro-aware window controls (`minimize` / `maximize` / `close` matching XFCE, GNOME, KDE Plasma, Cinnamon, MATE).
- **Messaging & Media:** Realtime WebSocket messaging, replies, reactions, mentions, file/photo/video/sticker/emoji sending, in-chat keyword/sender/date-range search, message pinning, labels, and hidden chats.
- **Voice & Video Calls (`zcall-*`):** Native Rust SRTP/ZRTP/RTP/RTCP call stack with PulseAudio/PipeWire audio, V4L2 camera capture, screen sharing, and P2P NAT traversal.
- **Linux Desktop Integration (`zalo-shell`):** StatusNotifierItem + DBusMenu system tray (with X11 XEmbed fallback), freedesktop notifications, XDG Desktop Portal file chooser / background / global shortcuts, and native screenshot region capture.
- **Optional OCR (`zocr`):** Local image text extraction powered by ONNX Runtime + PP-OCR models.

## Workspace Layout

```text
zalo-linux/
├── Cargo.toml         # Cargo workspace manifest
├── assets/            # Runtime fonts, UI images, call sounds, emoji tiles, sync2 WASM
├── crates/            # Rust workspace crates (zalo-*, zcall-*, zimage, zjxl, zocr)
├── packaging/         # .deb, .rpm, Arch PKGBUILD, AppImage, Flatpak, and .desktop files
└── vendor/            # Linux libc compatibility patch for xattr 0.2.3
```

## Build & Test

Requires **Rust 1.86+** (`rustup`) and standard Linux X11/Wayland/PulseAudio/OpenSSL development libraries.

```bash
# Universal build (Wayland + X11 + SNI/XEmbed tray)
cargo build --release -p zalo-gpui

# Minimal pure-Wayland build (omits X11 XEmbed tray fallback)
cargo build --release -p zalo-gpui --no-default-features

# Run all workspace tests
cargo test --workspace -- --test-threads=1
```

## Packaging

See [packaging/README.md](packaging/README.md) for `.deb`, `.rpm`, Arch `PKGBUILD`, `AppImage`, and `Flatpak` build instructions.
