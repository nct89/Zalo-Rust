# Zalo-Rust — macOS Port (`zalo-macos`)

Dedicated directory for porting and packaging **Zalo-Rust (GPUI)** for **macOS (Apple Silicon & Intel)**.

## Porting Roadmap

1. **Reuse 100% of shared crates in [../core](../core):** Add `zalo-macos/crates/*` to the root [../Cargo.toml](../Cargo.toml) workspace and depend on `zalo-core.workspace = true`, `zalo-protocol.workspace = true`, `zalo-client.workspace = true`, `zalo-storage.workspace = true`, `zalo-design.workspace = true`, `zcall-core.workspace = true`, `zimage.workspace = true`, `zjxl.workspace = true`, `zocr.workspace = true`. The `zalo-sync2` crate was removed with the PC-to-phone backup engine.
2. **Implement macOS platform crates:**
   - **Data paths (`zalo-core`):** `~/Library/Application Support/ZaloRust` and `~/Library/Caches/ZaloRust`.
   - **Desktop shell:** `NSStatusItem` (menu bar), `UNUserNotificationCenter` (notifications), `NSOpenPanel` / `NSSavePanel`, `SMAppService` (launch at login).
   - **Call audio:** CoreAudio `AudioUnit` (`VoiceProcessingIO` with hardware AEC) replacing `zalo-linux/crates/zcall-audio`.
   - **Camera & screen capture:** `AVFoundation` (`AVCaptureSession`) and `ScreenCaptureKit` (`SCStream`).
   - **UI:** GPUI Cocoa / Metal backend (`NSPasteboard`, native macOS traffic lights, standard window shadow/corners).
3. **Packaging:** `.app` bundle + `.dmg` (Universal Binary `aarch64 + x86_64`, code-signed & notarized).
