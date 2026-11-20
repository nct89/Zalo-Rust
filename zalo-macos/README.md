# Zalo-Rust — macOS Port (`zalo-macos`)

Dedicated workspace directory for porting and packaging **Zalo-Rust (GPUI)** for **macOS (Apple Silicon & Intel)**.

## Porting Roadmap from `../zalo-linux`

1. **Reuse 100% of pure-Rust crates:** `zalo-core`, `zalo-protocol`, `zalo-client`, `zalo-storage`, `zalo-sync2`, `zalo-design`, `zcall-core`, `zimage`, `zjxl`, `zocr`.
2. **Replace OS-specific integration layers:**
   - **Data paths (`zalo-core`):** `~/Library/Application Support/ZaloRust` and `~/Library/Caches/ZaloRust`.
   - **Desktop shell (`zalo-shell`):** `NSStatusItem` (menu bar), `UNUserNotificationCenter` (notifications), `NSOpenPanel` / `NSSavePanel`, `SMAppService` (launch at login).
   - **Call audio (`zcall-audio`):** CoreAudio `AudioUnit` (`VoiceProcessingIO` with hardware AEC) replacing PulseAudio/PipeWire.
   - **Camera & screen capture:** `AVFoundation` (`AVCaptureSession`) and `ScreenCaptureKit` (`SCStream`).
   - **UI (`zalo-gpui`):** GPUI Cocoa / Metal backend (`NSPasteboard`, native macOS traffic lights, standard window shadow/corners).
3. **Packaging:** `.app` bundle + `.dmg` (Universal Binary `aarch64 + x86_64`, code-signed & notarized).
