//! Linux desktop window caption controls detection (`min/max/close` order,
//! left/right placement, visual style, and active system icon-theme symbolic SVGs).
//!
//! Queries the active distro/desktop configuration:
//! - **XFCE**: `xfconf-query` (`xfwm4 /general/button_layout`, `xsettings /Gtk/DecorationLayout`, `/Net/IconThemeName`, `/Net/ThemeName`)
//! - **KDE Plasma**: `~/.config/kwinrc` (`ButtonsOnLeft` / `ButtonsOnRight`) & `~/.config/kdeglobals`
//! - **GNOME / Ubuntu / Fedora / Cinnamon / MATE / Budgie / Pantheon / COSMIC**:
//!   `gsettings` (`org.gnome.desktop.wm.preferences button-layout`, `icon-theme`, `gtk-theme`)
//!   and `~/.config/gtk-{3,4}.0/settings.ini`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::portal::{current_desktop, DesktopEnvironment};

/// Standard window titlebar action button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowButton {
    /// Minimize / hide window.
    Minimize,
    /// Maximize / restore window.
    Maximize,
    /// Close (or hide-to-tray) window.
    Close,
}

/// Visual button chrome matching the host Linux desktop / theme family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionStyle {
    /// Circular GTK / Adwaita / Yaru / XFCE CSD buttons (`26×26`, subtle circle hover, red close hover).
    LinuxRound,
    /// KDE Breeze circular icon buttons (crisp ring/circle highlight on hover).
    KdeBreeze,
    /// macOS-style traffic light dots (for WhiteSur / McMojave / macOS themes).
    MacTrafficLight,
}

/// Resolved window-control layout and icon theme for the host distro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowControlsLayout {
    /// Buttons rendered on the left side of the titlebar, left-to-right.
    pub left: Vec<WindowButton>,
    /// Buttons rendered on the right side of the titlebar, left-to-right.
    pub right: Vec<WindowButton>,
    /// Active system icon theme name (e.g. `elementary-xfce`, `Adwaita`, `Yaru`, `breeze`).
    pub icon_theme: Option<String>,
    /// Caption button visual style derived from desktop environment & theme.
    pub style: CaptionStyle,
}

impl Default for WindowControlsLayout {
    fn default() -> Self {
        Self {
            left: Vec::new(),
            right: vec![
                WindowButton::Minimize,
                WindowButton::Maximize,
                WindowButton::Close,
            ],
            icon_theme: None,
            style: CaptionStyle::LinuxRound,
        }
    }
}

/// Parse a GTK / GNOME / XFCE xsettings layout string such as
/// `"icon,menu:minimize,maximize,close"` or `"close,minimize,maximize:"`.
#[must_use]
pub fn parse_gtk_layout(raw: &str) -> (Vec<WindowButton>, Vec<WindowButton>) {
    let cleaned = raw.trim().trim_matches('\'').trim_matches('"');
    let (lhs, rhs) = cleaned.split_once(':').unwrap_or(("", cleaned));
    let parse_side = |side: &str| -> Vec<WindowButton> {
        let mut out = Vec::new();
        for tok in side.split(',') {
            let btn = match tok.trim().to_ascii_lowercase().as_str() {
                "minimize" => Some(WindowButton::Minimize),
                "maximize" => Some(WindowButton::Maximize),
                "close" => Some(WindowButton::Close),
                _ => None,
            };
            if let Some(b) = btn {
                if !out.contains(&b) {
                    out.push(b);
                }
            }
        }
        out
    };
    (parse_side(lhs), parse_side(rhs))
}

/// Parse an XFWM4 `/general/button_layout` string such as `"O|SHMC"` or `"CMH|O"`.
///
/// XFWM4 codes: `H` = Hide/Minimize, `M` = Maximize, `C` = Close, `|` = title separator.
#[must_use]
pub fn parse_xfwm_layout(raw: &str) -> (Vec<WindowButton>, Vec<WindowButton>) {
    let cleaned = raw.trim().trim_matches('\'').trim_matches('"');
    let (lhs, rhs) = cleaned.split_once('|').unwrap_or(("", cleaned));
    let parse_side = |side: &str| -> Vec<WindowButton> {
        let mut out = Vec::new();
        for ch in side.chars() {
            let btn = match ch.to_ascii_uppercase() {
                'H' => Some(WindowButton::Minimize),
                'M' => Some(WindowButton::Maximize),
                'C' => Some(WindowButton::Close),
                _ => None,
            };
            if let Some(b) = btn {
                if !out.contains(&b) {
                    out.push(b);
                }
            }
        }
        out
    };
    (parse_side(lhs), parse_side(rhs))
}

/// Parse KDE `kwinrc` `[org.kde.kdecoration2]` `ButtonsOnLeft` / `ButtonsOnRight`.
///
/// KDE codes: `I` = Minimize, `A` = Maximize, `X` = Close.
#[must_use]
pub fn parse_kde_layout(left: &str, right: &str) -> (Vec<WindowButton>, Vec<WindowButton>) {
    let parse_side = |side: &str| -> Vec<WindowButton> {
        let mut out = Vec::new();
        for ch in side.trim().chars() {
            let btn = match ch.to_ascii_uppercase() {
                'I' => Some(WindowButton::Minimize),
                'A' => Some(WindowButton::Maximize),
                'X' => Some(WindowButton::Close),
                _ => None,
            };
            if let Some(b) = btn {
                if !out.contains(&b) {
                    out.push(b);
                }
            }
        }
        out
    };
    (parse_side(left), parse_side(right))
}

/// Classify visual style from desktop environment and active GTK/WM theme name.
#[must_use]
pub fn classify_caption_style(desktop: DesktopEnvironment, theme_name: Option<&str>) -> CaptionStyle {
    if let Some(theme) = theme_name {
        let lower = theme.to_ascii_lowercase();
        if lower.contains("whitesur")
            || lower.contains("macos")
            || lower.contains("mojave")
            || lower.contains("bigsur")
            || lower.contains("sonoma")
            || lower.contains("sequoia")
            || lower.contains("catalina")
            || lower.contains("monterey")
            || lower.contains("ventura")
        {
            return CaptionStyle::MacTrafficLight;
        }
    }
    match desktop {
        DesktopEnvironment::Kde => CaptionStyle::KdeBreeze,
        _ => CaptionStyle::LinuxRound,
    }
}

fn ensure_has_close(left: &mut [WindowButton], right: &mut Vec<WindowButton>) {
    if !left.contains(&WindowButton::Close) && !right.contains(&WindowButton::Close) {
        right.push(WindowButton::Close);
    }
}

fn run_stdout(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn read_ini_value(path: &Path, section: Option<&str>, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_section = section.is_none();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = &line[1..line.len() - 1];
            in_section = section.is_none_or(|want| name.eq_ignore_ascii_case(want));
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case(key) {
                let val = v.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return Some(val.to_owned());
                }
            }
        }
    }
    None
}

fn read_gtk_ini(key: &str) -> Option<String> {
    let cfg = config_home();
    for rel in ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
        if let Some(val) = read_ini_value(&cfg.join(rel), Some("Settings"), key) {
            return Some(val);
        }
    }
    None
}

/// Detect the host Linux distro's window-control button layout, icon theme, and style.
#[must_use]
pub fn detect_window_controls() -> WindowControlsLayout {
    let desktop = current_desktop();
    let mut theme_name: Option<String> = None;
    let mut icon_theme: Option<String> = None;
    let mut parsed: Option<(Vec<WindowButton>, Vec<WindowButton>)> = None;

    match desktop {
        DesktopEnvironment::Xfce => {
            if let Some(raw) = run_stdout("xfconf-query", &["-c", "xfwm4", "-p", "/general/button_layout"]) {
                let pair = parse_xfwm_layout(&raw);
                if !pair.0.is_empty() || !pair.1.is_empty() {
                    parsed = Some(pair);
                }
            }
            if parsed.is_none() {
                if let Some(raw) =
                    run_stdout("xfconf-query", &["-c", "xsettings", "-p", "/Gtk/DecorationLayout"])
                {
                    let pair = parse_gtk_layout(&raw);
                    if !pair.0.is_empty() || !pair.1.is_empty() {
                        parsed = Some(pair);
                    }
                }
            }
            icon_theme = run_stdout("xfconf-query", &["-c", "xsettings", "-p", "/Net/IconThemeName"]);
            theme_name = run_stdout("xfconf-query", &["-c", "xsettings", "-p", "/Net/ThemeName"])
                .or_else(|| run_stdout("xfconf-query", &["-c", "xfwm4", "-p", "/general/theme"]));
        }
        DesktopEnvironment::Kde => {
            let cfg = config_home();
            let kwinrc = cfg.join("kwinrc");
            let l = read_ini_value(&kwinrc, Some("org.kde.kdecoration2"), "ButtonsOnLeft");
            let r = read_ini_value(&kwinrc, Some("org.kde.kdecoration2"), "ButtonsOnRight");
            if l.is_some() || r.is_some() {
                let pair = parse_kde_layout(l.as_deref().unwrap_or(""), r.as_deref().unwrap_or(""));
                if !pair.0.is_empty() || !pair.1.is_empty() {
                    parsed = Some(pair);
                }
            }
            let kdeglobals = cfg.join("kdeglobals");
            icon_theme = read_ini_value(&kdeglobals, Some("Icons"), "Theme");
            theme_name = read_ini_value(&kdeglobals, Some("General"), "Name");
        }
        DesktopEnvironment::Cinnamon => {
            if let Some(raw) =
                run_stdout("gsettings", &["get", "org.cinnamon.desktop.wm.preferences", "button-layout"])
            {
                let pair = parse_gtk_layout(&raw);
                if !pair.0.is_empty() || !pair.1.is_empty() {
                    parsed = Some(pair);
                }
            }
            icon_theme = run_stdout("gsettings", &["get", "org.cinnamon.desktop.interface", "icon-theme"])
                .map(|s| s.trim_matches('\'').to_owned());
            theme_name = run_stdout("gsettings", &["get", "org.cinnamon.desktop.interface", "gtk-theme"])
                .map(|s| s.trim_matches('\'').to_owned());
        }
        DesktopEnvironment::Mate => {
            if let Some(raw) =
                run_stdout("gsettings", &["get", "org.mate.Marco.general", "button-layout"])
            {
                let pair = parse_gtk_layout(&raw);
                if !pair.0.is_empty() || !pair.1.is_empty() {
                    parsed = Some(pair);
                }
            }
            icon_theme = run_stdout("gsettings", &["get", "org.mate.interface", "icon-theme"])
                .map(|s| s.trim_matches('\'').to_owned());
            theme_name = run_stdout("gsettings", &["get", "org.mate.interface", "gtk-theme"])
                .map(|s| s.trim_matches('\'').to_owned());
        }
        _ => {}
    }

    // Generic GNOME / GTK gsettings & settings.ini fallbacks
    if parsed.is_none() {
        if let Some(raw) =
            run_stdout("gsettings", &["get", "org.gnome.desktop.wm.preferences", "button-layout"])
        {
            let pair = parse_gtk_layout(&raw);
            if !pair.0.is_empty() || !pair.1.is_empty() {
                parsed = Some(pair);
            }
        }
    }
    if parsed.is_none() {
        if let Some(raw) = read_gtk_ini("gtk-decoration-layout") {
            let pair = parse_gtk_layout(&raw);
            if !pair.0.is_empty() || !pair.1.is_empty() {
                parsed = Some(pair);
            }
        }
    }
    if icon_theme.is_none() {
        icon_theme = run_stdout("gsettings", &["get", "org.gnome.desktop.interface", "icon-theme"])
            .map(|s| s.trim_matches('\'').to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| read_gtk_ini("gtk-icon-theme-name"));
    }
    if theme_name.is_none() {
        theme_name = run_stdout("gsettings", &["get", "org.gnome.desktop.interface", "gtk-theme"])
            .map(|s| s.trim_matches('\'').to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| read_gtk_ini("gtk-theme-name"));
    }

    let (mut left, mut right) = parsed.unwrap_or_else(|| {
        (
            Vec::new(),
            vec![
                WindowButton::Minimize,
                WindowButton::Maximize,
                WindowButton::Close,
            ],
        )
    });
    ensure_has_close(&mut left, &mut right);

    let style = classify_caption_style(desktop, theme_name.as_deref());
    WindowControlsLayout {
        left,
        right,
        icon_theme,
        style,
    }
}

/// Locate a standard FreeDesktop symbolic window control SVG (e.g.
/// `window-minimize-symbolic`, `window-maximize-symbolic`,
/// `window-restore-symbolic`, `window-close-symbolic`) from the active system
/// icon theme, falling back to installed standard Linux icon themes.
#[must_use]
pub fn find_window_icon_svg(icon_theme: Option<&str>, icon_name: &str) -> Option<PathBuf> {
    let mut themes: Vec<&str> = Vec::new();
    if let Some(t) = icon_theme.map(str::trim).filter(|s| !s.is_empty()) {
        themes.push(t);
    }
    for fallback in [
        "Adwaita",
        "elementary-xfce",
        "Yaru",
        "breeze",
        "Papirus",
        "Mint-Y",
        "hicolor",
    ] {
        if !themes.iter().any(|t| t.eq_ignore_ascii_case(fallback)) {
            themes.push(fallback);
        }
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.push(home.join(".local/share/icons"));
        roots.push(home.join(".icons"));
    }
    roots.push(PathBuf::from("/usr/local/share/icons"));
    roots.push(PathBuf::from("/usr/share/icons"));

    let subdirs = [
        "actions/symbolic",
        "symbolic/ui",
        "symbolic/actions",
        "ui/symbolic",
        "actions/16",
        "actions/22",
        "16x16/actions",
        "scalable/actions",
    ];
    let file = format!("{icon_name}.svg");
    let non_sym = icon_name
        .strip_suffix("-symbolic")
        .map(|base| format!("{base}.svg"));

    for theme in themes {
        for root in &roots {
            let base = root.join(theme);
            if !base.is_dir() {
                continue;
            }
            for sub in subdirs {
                let candidate = base.join(sub).join(&file);
                if candidate.is_file() {
                    return Some(candidate);
                }
                if let Some(alt) = non_sym.as_ref() {
                    let alt_cand = base.join(sub).join(alt);
                    if alt_cand.is_file() {
                        return Some(alt_cand);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gtk_gnome_ubuntu_and_elementary_layouts() {
        assert_eq!(
            parse_gtk_layout("'icon,menu:minimize,maximize,close'"),
            (
                vec![],
                vec![
                    WindowButton::Minimize,
                    WindowButton::Maximize,
                    WindowButton::Close,
                ]
            )
        );
        assert_eq!(
            parse_gtk_layout("close,minimize,maximize:"),
            (
                vec![
                    WindowButton::Close,
                    WindowButton::Minimize,
                    WindowButton::Maximize,
                ],
                vec![]
            )
        );
        assert_eq!(
            parse_gtk_layout("close:maximize"),
            (vec![WindowButton::Close], vec![WindowButton::Maximize])
        );
        assert_eq!(
            parse_gtk_layout("appmenu:close"),
            (vec![], vec![WindowButton::Close])
        );
    }

    #[test]
    fn parses_xfwm4_and_kde_layouts() {
        assert_eq!(
            parse_xfwm_layout("O|SHMC"),
            (
                vec![],
                vec![
                    WindowButton::Minimize,
                    WindowButton::Maximize,
                    WindowButton::Close,
                ]
            )
        );
        assert_eq!(
            parse_xfwm_layout("CMH|O"),
            (
                vec![
                    WindowButton::Close,
                    WindowButton::Maximize,
                    WindowButton::Minimize,
                ],
                vec![]
            )
        );
        assert_eq!(
            parse_kde_layout("M", "IAX"),
            (
                vec![],
                vec![
                    WindowButton::Minimize,
                    WindowButton::Maximize,
                    WindowButton::Close,
                ]
            )
        );
        assert_eq!(
            parse_kde_layout("XIA", ""),
            (
                vec![
                    WindowButton::Close,
                    WindowButton::Minimize,
                    WindowButton::Maximize,
                ],
                vec![]
            )
        );
    }

    #[test]
    fn classifies_caption_style_by_desktop_and_theme() {
        assert_eq!(
            classify_caption_style(DesktopEnvironment::Xfce, Some("Xfce")),
            CaptionStyle::LinuxRound
        );
        assert_eq!(
            classify_caption_style(DesktopEnvironment::Kde, Some("Breeze")),
            CaptionStyle::KdeBreeze
        );
        assert_eq!(
            classify_caption_style(DesktopEnvironment::Gnome, Some("WhiteSur-Light")),
            CaptionStyle::MacTrafficLight
        );
    }
}
