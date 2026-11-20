# Zalo-Rust — Windows Port (`zalo-windows`)

Dedicated workspace directory for porting and packaging **Zalo-Rust (GPUI)** for **Windows (x86_64 / ARM64)**.

## Porting Roadmap from `../zalo-linux`

1. **Reuse 100% of pure-Rust crates:** `zalo-core`, `zalo-protocol`, `zalo-client`, `zalo-storage`, `zalo-sync2`, `zalo-design`, `zcall-core`, `zimage`, `zjxl`, `zocr`.
2. **Replace OS-specific integration layers:**
   - **Data paths (`zalo-core`):** `%APPDATA%\ZaloRust` and `%LOCALAPPDATA%\ZaloRust`.
   - **Desktop shell (`zalo-shell`):** Win32 `Shell_NotifyIconW` (system tray), Windows Toast Notifications, Windows Startup Registry / Task Scheduler, native file dialogs.
   - **Call audio (`zcall-audio`):** WASAPI (shared mode 16 kHz mono + AEC/NS) replacing PulseAudio/PipeWire.
   - **Camera & screen capture:** Windows Media Foundation (`IMFSourceReader`) and DXGI Desktop Duplication.
   - **UI (`zalo-gpui`):** GPUI DirectX 11 / Win32 backend (dropping Linux `x11rb` / `x11_frame.rs`).
3. **Packaging:** `MSI` / `NSIS` / portable `.exe` with no Electron or WebView2 dependency.
