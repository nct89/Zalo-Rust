# Zalo-Rust — Cross-Platform Native Zalo Desktop Client (Core)

This repository contains the clean, native Rust + GPUI source code for **Zalo Desktop**, organized for cross-platform development across Linux, Windows, and macOS.

```text
Zalo-Rust/
├── zalo-linux/      # Complete Linux implementation (X11/Wayland, PulseAudio/PipeWire, V4L2, XDG Portal, GPUI)
├── zalo-windows/    # Target workspace skeleton for Windows (Win32/DirectX GPUI, WASAPI, DXGI, Media Foundation)
└── zalo-macos/      # Target workspace skeleton for macOS (Cocoa/Metal GPUI, CoreAudio, AVFoundation, ScreenCaptureKit)
```

## Shared Cross-Platform Architecture

The core pure-Rust crates inside `zalo-linux/crates/` are decoupled from OS-specific APIs and can be reused directly when porting to `zalo-windows/` and `zalo-macos/`:

| Crate | OS Dependency | Responsibility |
|---|---|---|
| `zalo-core` | None (pure Rust) | XDG/platform paths, app version constants, shared utilities |
| `zalo-protocol` | None (pure Rust) | AES/CBC encryption, ParamCipher v2, request signing, WebSocket frames |
| `zalo-client` | None (pure Rust) | REST API client, QR login flow, realtime WebSocket, media/file transfers |
| `zalo-storage` | None (SQLite) | Local database for conversations, messages, full-text & date-range search, ZDB4 decryption |
| `zalo-sync2` | None (`wasmi`) | Phone-to-desktop message sync v2 decryption and record decoding |
| `zcall-core` | None (pure Rust) | Call engine state machine, ZRTP/SRTP/RTP/RTCP, P2P NAT traversal, bitrate control |
| `zimage`, `zjxl`, `zocr` | None / ONNX Runtime | Thumbnail resizing, JPEG XL decoding/encoding, PP-OCR text recognition |
| `zalo-design` | None (pure Rust) | Design tokens, layout constants, and localized UI string tables |
| `zalo-shell` | Linux (`zbus`, `x11rb`) | System tray (SNI/DBusMenu/XEmbed), notifications, XDG portals, distro caption controls |
| `zcall-audio` | Linux (PulseAudio/PipeWire) | Audio capture & playback, echo/noise handling, jitter buffer, WSOLA |
| `zcall-engine` | Linux (V4L2/PipeWire) | Call media engine bridging `zcall-core` with audio/video devices |
| `zalo-gpui` | GPUI (`x11rb` on Linux) | Native GPU-accelerated desktop UI |

## Quick Build & Test (Linux)

```bash
cd zalo-linux

# Build optimized release binary
cargo build --release -p zalo-gpui

# Run the unit test suite (single-threaded for V4L2 device tests)
cargo test --workspace -- --test-threads=1

# Run the client
./target/release/zalo-gpui
```

See [zalo-linux/README.md](zalo-linux/README.md) and [zalo-linux/packaging/README.md](zalo-linux/packaging/README.md) for system dependencies and Linux packaging instructions.

## License

MIT — see [LICENSE](LICENSE).
