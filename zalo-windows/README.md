# Zalo-Rust — Windows (`zalo-windows`)

The GPUI client in `zalo-linux/crates/zalo-gpui` is also the native Windows app (DirectX 11 / DirectWrite / Win32). On `target_os = "windows"` it links the crates in this directory instead of the Linux D-Bus/X11/PulseAudio/V4L2 code. Shared protocol, storage, sync, backup, and codecs remain in `core/`.

## Build

Requirements: **Windows 10/11 (x86_64 or ARM64)**, **Rust 1.85+** (`x86_64-pc-windows-msvc`), **Visual Studio Build Tools** (Desktop development with C++ + Windows 10/11 SDK with `fxc.exe`), **CMake**, and **NASM**.

```powershell
# Optional: install NASM & CMake via winget or choco
winget install -e --id NASM.NASM
winget install -e --id Kitware.CMake

# Run Windows platform crate unit tests
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
cargo test -p zalo-shell-win -p zcall-audio-win -p zcall-engine-win -- --test-threads=1

# Build release binary & portable .zip (plus optional Inno Setup installer if `iscc` is installed)
pwsh zalo-windows/packaging/build-windows.ps1
```

> **Why build on Windows (or GitHub Actions `windows-latest`)?**  
> `gpui 0.2.2` compiles its DirectX 11 HLSL shaders in `build.rs` using Microsoft's `fxc.exe` from the Windows SDK when building `--release`. As a result, release binaries must be compiled on a Windows host or on the `windows-latest` CI runner.

Data lives in `%APPDATA%\ZaloRust` / `%LOCALAPPDATA%\ZaloRust` (resolved via `zalo-core::AppPaths`). Launch-at-login registers `"ZaloRust"` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

## What the Windows crates do

| Crate | Role |
|---|---|
| `zalo-shell-win` | Win32 notification-area tray (`Shell_NotifyIconW`) with dynamic unread badge `HICON` & native right-click menu, `WM_TASKBARCREATED` recovery, `NIF_INFO` tray notifications, COM `IFileOpenDialog` file/folder pickers, Named Pipe single-instance IPC (`\\.\pipe\zalo-rust-zalo-gpui`), `HKCU\...\Run` autostart, and `RegisterHotKey` global shortcuts |
| `zcall-audio-win` | WASAPI shared-mode capture and playout via `cpal`, resampled to 16 kHz mono `i16` for calls |
| `zcall-engine-win` | Opus encode/decode + multi-source jitter mixer bridge used by the call window |

## Windows UI & Media Integration

- **Window chrome**: Native DWM shadows, Windows 11 rounded corners, `WM_NCHITTEST` (`HTCAPTION`) title-bar dragging & Aero Snap, and right-aligned Windows 10/11 rectangular caption controls (`Min` / `Max` / red-hover `Close`).
- **Built-in Screenshot Overlay**: Native Win32 GDI `BitBlt` + `DwmGetWindowAttribute` window-bound detection feeding the shared GPUI crop & 1-click window-snap overlay (no external binaries needed).
- **Screen Sharing**: Native Win32 GDI multi-monitor capture (`EnumDisplayMonitors` + `BitBlt`) encoded with OpenH264 (no `ffmpeg` required).
- **Camera**: Uses `ffmpeg -f dshow` (install with `winget install Gyan.FFmpeg`) encoded with OpenH264.

## Future Enhancements

- Native Windows Media Foundation (`IMFSourceReader`) camera capture without `ffmpeg`
- DXGI Desktop Duplication for GPU-accelerated 60 FPS / HDR screen sharing
- Code-signed MSIX / Inno Setup installer with WinRT Toast AUMID registration
