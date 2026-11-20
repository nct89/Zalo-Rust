# Linux Packaging & Build Matrix

## Build Profiles (Cargo Features)

| Target | Build Command | Tray & Background Integration |
|---|---|---|
| **Default (Universal)** | `cargo build --release -p zalo-gpui` | SNI + DBusMenu (auto-reconnects on watcher restart), automatic X11 XEmbed fallback (`tray-xembed`), Dock Quicklist (`Actions=`), notification click wakeup, XDG Portal Background. |
| **Minimal Pure-Wayland** | `cargo build --release -p zalo-gpui --no-default-features` | Drops `x11rb` XEmbed from `zalo-shell`; keeps pure-Rust SNI + DBusMenu + Dock Quicklist + Portal Background. |

## Distribution Packaging

- **Debian / Ubuntu / Linux Mint / Pop!_OS (`.deb`)**: run `./packaging/deb/build-deb.sh` (declares `Recommends: gnome-shell-extension-appindicator | gir1.2-ayatanaappindicator3-0.1`, compresses with `-Zxz`, strips the binary, and bundles runtime fonts/sounds into `/usr/share/zalo-rust/`).
- **Fedora / openSUSE (`.rpm`)**: use `packaging/rpm/zalo-linux.spec` (declares `Recommends: gnome-shell-extension-appindicator`).
- **Arch Linux (`PKGBUILD`)**: use `packaging/arch/PKGBUILD` (declares `optdepends` for GNOME AppIndicator).
- **AppImage**: run `./packaging/appimage/build-appimage.sh`.
- **Flatpak**: use `packaging/flatpak/io.github.nct89.ZaloLinux.yml` (grants `--talk-name=org.kde.StatusNotifierWatcher`, `--own-name=org.kde.StatusNotifierItem-*`, `--talk-name=org.freedesktop.Notifications`, `--talk-name=org.freedesktop.portal.Background`).

## Interactive Installer & Optional OCR Pack

Run `zalo-gpui --install` (or `--setup`) to open the interactive setup utility:
1. Detects `/etc/os-release` and checks distro dependencies (`.deb` / `.rpm` / `pacman`).
2. Installs the optimized binary, `.desktop` entry, application icons (32–256px), `Zalo-Segoe-UI` / `zalo-font` fonts, and call ringtones.
3. Optionally downloads the OCR runtime + models archive (`.tar.zst`) from `ZALO_OCR_MANIFEST_URL` (defaults to the GitHub Release OCR manifest), verifies `SHA-256`, extracts into `~/.local/share/zalo-rust/ocr`, and validates ONNX Runtime initialization.
