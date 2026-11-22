# Zalo-Rust — macOS (`zalo-macos`)

The GPUI client in `zalo-linux/crates/zalo-gpui` is the macOS app. On `target_os = "macos"` it links the crates in this directory instead of the Linux tray, PulseAudio, and V4L2 code. Shared protocol, storage, sync, and backup stay in `core/`.

## Build

Requirements: macOS 11+, Rust 1.85+, Xcode command-line tools, CMake, NASM.

```bash
brew install cmake nasm pkg-config ffmpeg
cargo test -p zalo-shell-mac -p zcall-audio-mac -p zcall-engine-mac
cargo build --release -p zalo-gpui
VERSION=0.1.0 bash zalo-macos/packaging/build-app.sh
open dist/Zalo.app
```

`build-app.sh` writes `dist/Zalo.app` and a zip. The bundle is ad-hoc signed so Apple Silicon can launch it. Notarization needs an Apple Developer ID and is not part of this script.

Data lives in `~/Library/Application Support/zalo-rust`. Launch at login writes `~/Library/LaunchAgents/io.github.nct89.ZaloRust.plist`.

## What the macOS crates do

| Crate | Role |
|---|---|
| `zalo-shell-mac` | Menu bar item, Dock badge, Notification Center, `osascript` open/save panels, `screencapture -i`, single-instance socket, LaunchAgent |
| `zcall-audio-mac` | CoreAudio capture and playback through `cpal`, resampled to 16 kHz mono for calls |
| `zcall-engine-mac` | Opus encode/decode bridge used by the call window |

The window is still the shared GPUI UI. Caption buttons are traffic lights on the left. Clipboard writes go through GPUI's pasteboard. Screenshot uses the system region tool. Camera and screen share spawn `ffmpeg` with AVFoundation (`-f avfoundation`) and encode H.264 with OpenH264.

## Still outside this port

- Hardware echo cancellation (`VoiceProcessingIO`) — calls use the CoreAudio device directly, with digital gain only
- Screen capture without `ffmpeg` (`ScreenCaptureKit`)
- A universal `arm64 + x86_64` binary and notarized `.dmg`
