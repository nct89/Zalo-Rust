Name:           zalo-linux
Version:        0.1.0
Release:        1%{?dist}
Summary:        Native Rust Zalo Desktop client (GPUI)
License:        MIT
URL:            https://github.com/nct89/Zalo-Rust

BuildRequires:  cargo >= 1.86
BuildRequires:  rust >= 1.86
Recommends:     gnome-shell-extension-appindicator
Recommends:     libnotify
Recommends:     xdg-desktop-portal

%description
Native Linux Zalo desktop client written in Rust with StatusNotifierItem,
com.canonical.dbusmenu, X11 XEmbed fallback, Dock Quicklist actions, and XDG
Background portal support.

%build
cargo build --release -p zalo-gpui --features tray-xembed

%install
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
install -Dm755 "${TARGET_DIR}/release/zalo-gpui" %{buildroot}%{_bindir}/zalo-gpui
strip --strip-all %{buildroot}%{_bindir}/zalo-gpui || true
bash zalo-linux/packaging/linux/stage-runtime-assets.sh %{buildroot}%{_datadir}/zalo-rust
install -Dm644 zalo-linux/packaging/linux/zalo-gpui.desktop %{buildroot}%{_datadir}/applications/zalo-gpui.desktop
install -Dm644 zalo-linux/crates/zalo-shell/assets/zalo-256.png %{buildroot}%{_datadir}/icons/hicolor/256x256/apps/zalo-gpui.png

%files
%{_bindir}/zalo-gpui
%{_datadir}/zalo-rust
%{_datadir}/applications/zalo-gpui.desktop
%{_datadir}/icons/hicolor/256x256/apps/zalo-gpui.png
