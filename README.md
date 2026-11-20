# Zalo-Rust — Cross-Platform Native Zalo Desktop Client (Core)

This repository contains the clean, native Rust + GPUI source code for **Zalo Desktop**, structured as a single unified Cargo workspace so OS-independent protocol/client crates are shared across Linux, Windows, and macOS.

```text
Zalo-Rust/
├── Cargo.toml         # Unified cross-platform Rust workspace manifest
├── protocol.json      # Canonical Zalo protocol parameters (zpw_ver, api_type, User-Agent)
├── core/              # 10 OS-independent pure-Rust crates shared by all platforms
├── assets/            # Shared runtime fonts, UI images, call sounds, emoji tiles, sync2 WASM
├── vendor/            # Linux libc compatibility patch for xattr 0.2.3
├── zalo-linux/        # Linux platform crates (zalo-shell, zcall-audio, zcall-engine, zalo-gpui) + packaging
├── zalo-windows/      # Target workspace directory for Windows port (Win32/DirectX GPUI, WASAPI, DXGI)
└── zalo-macos/        # Target workspace directory for macOS port (Cocoa/Metal GPUI, CoreAudio, AVFoundation)
```

## Shared Cross-Platform Architecture

All pure-Rust protocol, storage, media, and call-engine crates live in `core/` so upstream Zalo updates only need to be applied **once** for Linux, Windows, and macOS:

| Location | Crate | OS Dependency | Responsibility |
|---|---|---|---|
| `core/` | `zalo-core` | None (pure Rust) | XDG/platform paths, app version utilities |
| `core/` | `zalo-protocol` | None (pure Rust) | AES/CBC, ParamCipher v2, request signing, dynamic `protocol.json` overrides |
| `core/` | `zalo-client` | None (pure Rust) | REST API client, QR login flow, realtime WebSocket, media/file transfers |
| `core/` | `zalo-storage` | None (SQLite) | Local database for conversations, messages, search, ZDB4 decryption |
| `core/` | `zalo-sync2` | None (`wasmi`) | Phone-to-desktop message sync v2 decryption & record decoding |
| `core/` | `zalo-design` | None (pure Rust) | Design tokens, layout constants, localized UI string tables |
| `core/` | `zcall-core` | None (pure Rust) | Call engine state machine, ZRTP/SRTP/RTP/RTCP, P2P NAT traversal, bitrate control |
| `core/` | `zimage`, `zjxl`, `zocr` | None / ONNX Runtime | Thumbnail resizing, JPEG XL codec, PP-OCR text recognition |
| `zalo-linux/crates/` | `zalo-shell` | Linux (`zbus`, `x11rb`) | System tray (SNI/DBusMenu/XEmbed), notifications, XDG portals, distro caption controls |
| `zalo-linux/crates/` | `zcall-audio`, `zcall-engine` | Linux (PulseAudio/PipeWire/V4L2) | Call audio/video device I/O bridging `zcall-core` |
| `zalo-linux/crates/` | `zalo-gpui` | Linux GPUI (`x11rb`) | Native GPU-accelerated desktop UI |

## Quick Build & Test (Linux)

```bash
# Build optimized release binary (works from repo root or zalo-linux/)
cargo build --release -p zalo-gpui

# Run all workspace unit tests (single-threaded for V4L2 device tests)
cargo test --workspace -- --test-threads=1

# Run the client
./target/release/zalo-gpui
```

## Runtime Protocol Overrides (`protocol.json`)

When Zalo bumps its desktop `zpw_ver` (`client_version`) or `User-Agent`, you can hot-patch without recompiling by placing a `protocol.json` file at `~/.config/zalo-rust/protocol.json` (see [protocol.json](protocol.json)) or setting environment variables:

- `ZALO_CLIENT_VERSION` (overrides `zpw_ver` / `client_version`)
- `ZALO_API_TYPE` (overrides `type` / `zpw_type`)
- `ZALO_USER_AGENT` (overrides HTTP/WebSocket `User-Agent`)
- `ZALO_PARAM_CIPHER_KEY` (overrides 32-byte `ParamCipher` v2 key)

## Automation (GitHub Actions)

- **CI ([ci.yml](.github/workflows/ci.yml)):** Runs full workspace tests and release build on every push & PR.
- **Release ([release.yml](.github/workflows/release.yml)):** On `v*` tags, builds & publishes `.deb`, portable Linux `.tar.gz`, `protocol.json`, and `SHA256SUMS.txt`.
- **Upstream Watcher ([zalo-watcher.yml](.github/workflows/zalo-watcher.yml)):** Checks daily for new upstream Zalo Desktop releases and automatically opens a tracking issue with a protocol verification checklist.

## License

MIT — see [LICENSE](LICENSE).
