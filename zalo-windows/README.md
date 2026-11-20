# Zalo-Rust — Windows Port (`zalo-windows`)

Dedicated directory for porting and packaging **Zalo-Rust (GPUI)** for **Windows (x86_64 / ARM64)**.

## Porting Roadmap

1. **Reuse 100% of shared crates in [../core](../core):** Add `zalo-windows/crates/*` to the root [../Cargo.toml](../Cargo.toml) workspace and depend on `zalo-core.workspace = true`, `zalo-protocol.workspace = true`, `zalo-client.workspace = true`, `zalo-storage.workspace = true`, `zalo-sync2.workspace = true`, `zalo-design.workspace = true`, `zcall-core.workspace = true`, `zimage.workspace = true`, `zjxl.workspace = true`, `zocr.workspace = true`.
2. **Implement Windows platform crates:**
   - **Data paths (`zalo-core`):** `%APPDATA%\ZaloRust` and `%LOCALAPPDATA%\ZaloRust`.
   - **Desktop shell:** Win32 `Shell_NotifyIconW` (system tray), Windows Toast Notifications, Startup Registry / Task Scheduler, native file dialogs.
   - **Call audio:** WASAPI (shared mode 16 kHz mono + AEC/NS) replacing `zalo-linux/crates/zcall-audio`.
   - **Camera & screen capture:** Windows Media Foundation (`IMFSourceReader`) and DXGI Desktop Duplication.
   - **UI:** GPUI DirectX 11 / Win32 backend sharing UI views with `zalo-linux` without Linux `x11rb` / `x11_frame.rs`.
3. **Packaging:** `MSI` / `NSIS` / portable `.exe` with no Electron or WebView2 dependency.
