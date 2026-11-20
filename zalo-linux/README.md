# Zalo-Rust for Linux (`zalo-linux`)

Linux platform layer and packaging for **Zalo-Rust**, built on top of the shared pure-Rust crates in [../core](../core) and shared runtime assets in [../assets](../assets).

## Layout

```text
zalo-linux/
├── crates/
│   ├── zalo-shell/    # System tray (SNI/DBusMenu/XEmbed), notifications, XDG portals, distro controls
│   ├── zcall-audio/   # PulseAudio / PipeWire audio capture & playout
│   ├── zcall-engine/  # V4L2 camera & call media engine
│   └── zalo-gpui/     # Native GPUI desktop GUI (Wayland + X11)
└── packaging/         # .deb, .rpm, Arch PKGBUILD, AppImage, Flatpak, and .desktop files
```

## Build & Test

Requires **Rust 1.86+** (`rustup`) and standard Linux X11/Wayland/PulseAudio/OpenSSL development libraries. Run from either the repository root or `zalo-linux/`:

```bash
# Universal build (Wayland + X11 + SNI/XEmbed tray)
cargo build --release -p zalo-gpui --features tray-xembed

# Minimal pure-Wayland build (omits X11 XEmbed tray fallback)
cargo build --release -p zalo-gpui --no-default-features

# Run all workspace tests
cargo test --workspace -- --test-threads=1
```

See [packaging/README.md](packaging/README.md) for `.deb`, `.rpm`, Arch `PKGBUILD`, `AppImage`, and `Flatpak` build instructions.
