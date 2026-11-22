//! Local screen share for a Zalo call.
//!
//! Supports both:
//! - **Wayland (`org.freedesktop.portal.ScreenCast` + PipeWire)**: opens the
//!   native compositor picker (GNOME/KDE/Hyprland/Sway/Flatpak) via `zalo-shell`,
//!   streams frames from the PipeWire remote FD via GStreamer `pipewiresrc`
//!   (with `grim` fallback on wlroots).
//! - **X11 (`x11rb` `GetImage`)**: grabs physical monitors (RandR), screen
//!   regions, or individual windows in pure Rust with 2-pass `INTER_AREA`
//!   downscaling so desktop text remains sharp.
//!
//! Both paths use adaptive frame-rate pacing (`4..=15 fps`) with fast luma
//! frame-diff detection so motion stays smooth while static slides/code save
//! CPU and uplink bandwidth, encoded as H.264 Baseline Annex-B via OpenH264.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use openh264::formats::YUVBuffer;
use openh264::Timestamp;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, ImageFormat, MapState, Window};
use x11rb::rust_connection::RustConnection;

use crate::call_camera::{CamEvent, LocalPreview};

const MAX_WIDTH: usize = 1280;
const MAX_HEIGHT: usize = 720;
/// Matches `call_video::PREVIEW_MAX_EDGE` (fits inside GPUI's 1024 px atlas
/// while keeping 16:9 screen previews crisp at `960×540`).
const PREVIEW_MAX_EDGE: usize = 960;
/// Active motion capture/encode ceiling (fps).
const MAX_FPS: u32 = 15;
/// Static screen capture cadence (fps) when no pixels changed.
const MIN_FPS: u32 = 4;
/// How long to stay at `MAX_FPS` after the last detected motion.
const MOTION_HOLD: Duration = Duration::from_millis(1_500);
/// Maximum gap before sending a keepalive P-frame on a completely static screen.
const STATIC_KEEPALIVE: Duration = Duration::from_secs(1);
const BITRATE_BPS: u32 = 1_200_000;
const KEY_INTERVAL: Duration = Duration::from_secs(5);

/// Screen / monitor / region / window target selected by the user before sharing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShareTarget {
    /// Interactive Wayland XDG Desktop Portal (`ScreenCast` + PipeWire) picker.
    WaylandPortal,
    /// Entire primary display (fallback when X11 geometry is unavailable).
    #[default]
    FullScreen,
    /// Fixed sub-rectangle of the X11 root display (physical monitor or region).
    Region {
        /// Human-readable label shown in picker and banner.
        label: String,
        /// Left X offset on root window.
        x: i16,
        /// Top Y offset on root window.
        y: i16,
        /// Width in pixels.
        w: u16,
        /// Height in pixels.
        h: u16,
    },
    /// Specific X11 top-level window tracked dynamically by window ID.
    Window {
        /// X11 window ID.
        id: Window,
        /// Window title shown in picker and banner.
        title: String,
        /// Last known width in pixels.
        w: u16,
        /// Last known height in pixels.
        h: u16,
    },
}

impl ShareTarget {
    /// Short label for the share-picker row and active sharing banner.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::WaylandPortal => "Chọn màn hình / cửa sổ (Wayland)".into(),
            Self::FullScreen => "Toàn màn hình".into(),
            Self::Region { label, .. } => label.clone(),
            Self::Window { title, .. } => title.clone(),
        }
    }

    /// Secondary detail line (kind + dimensions) for the share-picker UI.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::WaylandPortal => "Hộp thoại hệ thống XDG ScreenCast Portal • PipeWire".into(),
            Self::FullScreen => "Toàn bộ màn hình hiện tại".into(),
            Self::Region { label, w, h, .. } => {
                if label.starts_with("Màn hình") || label.starts_with("Toàn màn hình") {
                    format!("Màn hình • {w}×{h}")
                } else {
                    format!("Vùng màn hình • {w}×{h}")
                }
            }
            Self::Window { w, h, .. } => format!("Cửa sổ • {w}×{h}"),
        }
    }
}

#[derive(Clone, Debug)]
struct MonitorRect {
    name: String,
    primary: bool,
    x: i16,
    y: i16,
    w: u16,
    h: u16,
}

fn list_randr_monitors(conn: &RustConnection, root: Window) -> Vec<MonitorRect> {
    let Ok(cookie) = conn.randr_get_monitors(root, true) else {
        return Vec::new();
    };
    let Ok(reply) = cookie.reply() else {
        return Vec::new();
    };
    let mut monitors: Vec<MonitorRect> = reply
        .monitors
        .into_iter()
        .filter(|m| m.width >= 320 && m.height >= 200)
        .map(|m| {
            let name = conn
                .get_atom_name(m.name)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| String::from_utf8_lossy(&r.name).trim().to_owned())
                .unwrap_or_default();
            MonitorRect {
                name,
                primary: m.primary,
                x: m.x,
                y: m.y,
                w: m.width,
                h: m.height,
            }
        })
        .collect();
    // Put primary monitor first, then top-to-bottom, left-to-right.
    monitors.sort_by(|a, b| {
        b.primary
            .cmp(&a.primary)
            .then_with(|| a.y.cmp(&b.y))
            .then_with(|| a.x.cmp(&b.x))
    });
    monitors
}

/// Lists selectable share targets: Wayland ScreenCast Portal (first when running
/// on Wayland with portal support), plus X11 monitors, half-screen regions, and
/// top-level windows when an X11/XWayland server is available.
pub(crate) fn list_share_targets() -> Vec<ShareTarget> {
    let mut targets = Vec::new();
    if zalo_shell::is_wayland_session() && zalo_shell::portal_screencast_available() {
        targets.push(ShareTarget::WaylandPortal);
    }
    let Ok((conn, screen_num)) = RustConnection::connect(None) else {
        if targets.is_empty() {
            targets.push(if zalo_shell::is_wayland_session() {
                ShareTarget::WaylandPortal
            } else {
                ShareTarget::FullScreen
            });
        }
        return targets;
    };
    let Some(screen) = conn.setup().roots.get(screen_num) else {
        if targets.is_empty() {
            targets.push(ShareTarget::FullScreen);
        }
        return targets;
    };
    let root = screen.root;
    let sw = screen.width_in_pixels;
    let sh = screen.height_in_pixels;

    let monitors = list_randr_monitors(&conn, root);
    let primary_rect = if monitors.is_empty() {
        targets.push(ShareTarget::Region {
            label: format!("Toàn màn hình ({sw}×{sh})"),
            x: 0,
            y: 0,
            w: sw,
            h: sh,
        });
        (0_i16, 0_i16, sw, sh)
    } else if monitors.len() == 1 {
        let m = &monitors[0];
        targets.push(ShareTarget::Region {
            label: format!("Toàn màn hình ({}×{})", m.w, m.h),
            x: m.x,
            y: m.y,
            w: m.w,
            h: m.h,
        });
        (m.x, m.y, m.w, m.h)
    } else {
        for (idx, m) in monitors.iter().enumerate() {
            let num = idx + 1;
            let suffix = if m.primary { " (Chính)" } else { "" };
            let label = if m.name.is_empty() {
                format!("Màn hình {num}{suffix}")
            } else {
                format!("Màn hình {num} — {}{suffix}", m.name)
            };
            targets.push(ShareTarget::Region {
                label,
                x: m.x,
                y: m.y,
                w: m.w,
                h: m.h,
            });
        }
        let p = &monitors[0];
        (p.x, p.y, p.w, p.h)
    };

    let (px, py, pw, ph) = primary_rect;
    if pw >= 1280 && ph >= 600 {
        let half_w = (pw / 2) & !1;
        targets.push(ShareTarget::Region {
            label: "Nửa màn hình trái".into(),
            x: px,
            y: py,
            w: half_w,
            h: ph,
        });
        targets.push(ShareTarget::Region {
            label: "Nửa màn hình phải".into(),
            x: px.saturating_add(half_w as i16),
            y: py,
            w: pw.saturating_sub(half_w),
            h: ph,
        });
    }

    for win in list_x11_windows(&conn, root) {
        if targets.len() >= 16 {
            break;
        }
        targets.push(win);
    }
    targets
}

fn list_x11_windows(conn: &RustConnection, root: Window) -> Vec<ShareTarget> {
    let mut out = Vec::new();
    let atom = |name: &[u8]| -> Option<u32> {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
            .filter(|&a| a != 0)
    };
    let Some(client_list_atom) = atom(b"_NET_CLIENT_LIST") else {
        return out;
    };
    let net_wm_name = atom(b"_NET_WM_NAME");
    let utf8_string = atom(b"UTF8_STRING");
    let net_wm_window_type = atom(b"_NET_WM_WINDOW_TYPE");
    let skip_types: Vec<u32> = [
        b"_NET_WM_WINDOW_TYPE_DESKTOP".as_slice(),
        b"_NET_WM_WINDOW_TYPE_DOCK".as_slice(),
        b"_NET_WM_WINDOW_TYPE_SPLASH".as_slice(),
        b"_NET_WM_WINDOW_TYPE_NOTIFICATION".as_slice(),
        b"_NET_WM_WINDOW_TYPE_TOOLTIP".as_slice(),
        b"_NET_WM_WINDOW_TYPE_POPUP_MENU".as_slice(),
    ]
    .into_iter()
    .filter_map(atom)
    .collect();

    let Some(prop) = conn
        .get_property(false, root, client_list_atom, AtomEnum::WINDOW, 0, 256)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return out;
    };
    let Some(ids) = prop.value32() else {
        return out;
    };
    for id in ids {
        let Some(attrs) = conn
            .get_window_attributes(id)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            continue;
        };
        if attrs.map_state != MapState::VIEWABLE {
            continue;
        }
        if let Some(wt_atom) = net_wm_window_type {
            if let Some(wt) = conn
                .get_property(false, id, wt_atom, AtomEnum::ATOM, 0, 16)
                .ok()
                .and_then(|c| c.reply().ok())
            {
                if wt
                    .value32()
                    .is_some_and(|mut vals| vals.any(|a| skip_types.contains(&a)))
                {
                    continue;
                }
            }
        }
        let Some(geom) = conn.get_geometry(id).ok().and_then(|c| c.reply().ok()) else {
            continue;
        };
        if geom.width < 120 || geom.height < 80 {
            continue;
        }
        let title = read_window_title(conn, id, net_wm_name, utf8_string);
        let title = title.trim();
        if title.is_empty()
            || title.eq_ignore_ascii_case("desktop")
            || title.starts_with("Zalo Call")
        {
            continue;
        }
        out.push(ShareTarget::Window {
            id,
            title: title.chars().take(60).collect(),
            w: geom.width,
            h: geom.height,
        });
    }
    out
}

fn read_window_title(
    conn: &RustConnection,
    win: Window,
    net_wm_name: Option<u32>,
    utf8_string: Option<u32>,
) -> String {
    if let (Some(prop), Some(ty)) = (net_wm_name, utf8_string) {
        if let Some(reply) = conn
            .get_property(false, win, prop, ty, 0, 128)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            if !reply.value.is_empty() {
                return String::from_utf8_lossy(&reply.value).into_owned();
            }
        }
    }
    if let Some(reply) = conn
        .get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 128)
        .ok()
        .and_then(|c| c.reply().ok())
    {
        if !reply.value.is_empty() {
            return String::from_utf8_lossy(&reply.value).into_owned();
        }
    }
    String::new()
}

/// Desktop capture encoded on `zcall-screen`.
pub(crate) struct ScreenCapture {
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<CamEvent>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
}

impl ScreenCapture {
    /// Starts a native X11 grab of `target`.
    ///
    /// # Errors
    ///
    /// Returns when the capture thread cannot start. Display errors are
    /// reported later as [`CamEvent::Failed`].
    pub(crate) fn start(target: ShareTarget) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let force_key = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::sync_channel(3);
        let preview = Arc::new(Mutex::new(None));
        let stop_t = Arc::clone(&stop);
        let key_t = Arc::clone(&force_key);
        let preview_t = Arc::clone(&preview);
        let handle = std::thread::Builder::new()
            .name("zcall-screen".into())
            .spawn(move || capture_loop(stop_t, key_t, tx, preview_t, target))
            .map_err(|err| format!("Không mở được chia sẻ màn hình: {err}"))?;
        Ok(Self {
            stop,
            force_key,
            handle: Some(handle),
            rx,
            preview,
        })
    }

    /// Requests an immediate IDR key frame (e.g. on peer RTCP PLI/FIR).
    pub(crate) fn request_key_frame(&self) {
        self.force_key.store(true, Ordering::Release);
    }

    /// Latest local preview of the shared desktop, if refreshed.
    pub(crate) fn take_preview(&self) -> Option<LocalPreview> {
        self.preview.lock().ok().and_then(|mut slot| slot.take())
    }

    /// The next encoded frame or failure.
    pub(crate) fn poll(&self) -> Option<CamEvent> {
        self.rx.try_recv().ok()
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Why a grab cannot start, before any capture thread is spawned.
pub(crate) fn grab_unavailable() -> Option<String> {
    if zalo_shell::is_wayland_session() {
        if zalo_shell::portal_screencast_available() || tool_exists("grim") {
            return None;
        }
        if std::env::var_os("DISPLAY").is_some() && RustConnection::connect(None).is_ok() {
            return None;
        }
        return Some(
            "Chia sẻ màn hình trên Wayland cần XDG ScreenCast Portal (xdg-desktop-portal) hoặc grim."
                .into(),
        );
    }
    if std::env::var_os("DISPLAY").is_none() {
        return Some("Chia sẻ màn hình cần phiên X11 (DISPLAY) hoặc Wayland Portal.".into());
    }
    if RustConnection::connect(None).is_err() {
        return Some("Không kết nối được máy chủ X11 để chụp màn hình.".into());
    }
    None
}

fn tool_exists(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

fn gst_pipewiresrc_available() -> bool {
    if !tool_exists("gst-launch-1.0") {
        return false;
    }
    if tool_exists("gst-inspect-1.0") {
        return Command::new("gst-inspect-1.0")
            .arg("pipewiresrc")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    }
    true
}

/// Fast sampled luma change detector: checks every 16th byte of the Y plane.
/// Returns `true` when frames differ in dimensions or >= 0.05% of sampled pixels
/// changed by >= 6 luma levels (catches typing, cursor/scroll, video playback
/// while ignoring tiny sensor/subpixel noise).
pub(crate) fn luma_changed(prev: &[u8], curr: &[u8]) -> bool {
    if prev.len() != curr.len() || curr.is_empty() {
        return true;
    }
    let step = 16usize;
    let total_samples = (curr.len() + step - 1) / step;
    let threshold = (total_samples / 2000).max(2);
    let mut diffs = 0usize;
    let mut idx = 0usize;
    while idx < curr.len() {
        if prev[idx].abs_diff(curr[idx]) >= 6 {
            diffs += 1;
            if diffs >= threshold {
                return true;
            }
        }
        idx += step;
    }
    false
}

/// Adaptive frame-rate pacer (`MIN_FPS`..=`MAX_FPS`) with static-frame skipping.
struct AdaptivePacer {
    last_motion: Instant,
    last_sent: Instant,
    prev_y: Vec<u8>,
}

impl AdaptivePacer {
    fn new(now: Instant) -> Self {
        Self {
            last_motion: now,
            last_sent: now - STATIC_KEEPALIVE,
            prev_y: Vec::new(),
        }
    }

    /// Evaluates the newly captured Y plane (`y_plane`), returning
    /// `(should_encode, next_interval)`.
    fn evaluate(&mut self, y_plane: &[u8], force_send: bool, now: Instant) -> (bool, Duration) {
        let changed = luma_changed(&self.prev_y, y_plane);
        if changed {
            self.last_motion = now;
            self.prev_y.clear();
            self.prev_y.extend_from_slice(y_plane);
        }
        let active = changed || now.duration_since(self.last_motion) < MOTION_HOLD;
        let next_interval = if active {
            Duration::from_secs_f64(1.0 / f64::from(MAX_FPS))
        } else {
            Duration::from_secs_f64(1.0 / f64::from(MIN_FPS))
        };
        let keepalive_due = now.duration_since(self.last_sent) >= STATIC_KEEPALIVE;
        let should_encode = changed || force_send || keepalive_due;
        if should_encode {
            self.last_sent = now;
        }
        (should_encode, next_interval)
    }

    fn reset(&mut self) {
        self.prev_y.clear();
    }
}

fn capture_loop(
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    tx: SyncSender<CamEvent>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
    target: ShareTarget,
) {
    let fail = |text: String| {
        let _ = tx.try_send(CamEvent::Failed(text));
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if matches!(target, ShareTarget::WaylandPortal) {
            capture_wayland_portal(&stop, &force_key, &tx, &preview);
        } else {
            capture_loop_inner(&stop, &force_key, &tx, &preview, &target);
        }
    }));
    if result.is_err() {
        fail("Chia sẻ màn hình bị lỗi và đã dừng. Cuộc gọi vẫn mở.".into());
    }
}

/// Captures Wayland screen/window via `org.freedesktop.portal.ScreenCast` +
/// PipeWire (`gst-launch-1.0 pipewiresrc`), falling back to `grim` on wlroots.
fn capture_wayland_portal(
    stop: &AtomicBool,
    force_key: &AtomicBool,
    tx: &SyncSender<CamEvent>,
    preview: &Mutex<Option<LocalPreview>>,
) {
    let fail = |text: String| {
        let _ = tx.try_send(CamEvent::Failed(text));
    };

    if zalo_shell::portal_screencast_available() {
        let session = match zalo_shell::start_screencast(zalo_shell::SOURCE_TYPE_BOTH) {
            Ok(sess) => sess,
            Err(err) => {
                if !stop.load(Ordering::Acquire) {
                    fail(err.to_string());
                }
                return;
            }
        };
        if stop.load(Ordering::Acquire) {
            return;
        }
        if !gst_pipewiresrc_available() {
            drop(session);
            if tool_exists("grim") {
                capture_grim_loop(stop, force_key, tx, preview);
                return;
            }
            fail(
                "Cần cài gói gstreamer1.0-tools và gstreamer1.0-pipewire để chia sẻ màn hình trên Wayland."
                    .into(),
            );
            return;
        }

        let src_w = session.stream.width.unwrap_or(MAX_WIDTH as u32) as usize;
        let src_h = session.stream.height.unwrap_or(MAX_HEIGHT as u32) as usize;
        let (enc_w, enc_h) = fit_screen_size(src_w, src_h);
        let (prev_w, prev_h) = fit_edge_even(enc_w, enc_h, PREVIEW_MAX_EDGE);
        let mut encoder = match open_screen_encoder(enc_w, enc_h) {
            Ok(enc) => enc,
            Err(_) => {
                fail("Không mở được bộ mã hình chia sẻ màn hình.".into());
                return;
            }
        };

        let raw_fd = session.pipewire_fd().as_raw_fd();
        let node_id = session.stream.node_id;
        let caps = format!(
            "video/x-raw,format=I420,width={enc_w},height={enc_h},framerate={MAX_FPS}/1"
        );
        let mut cmd = Command::new("gst-launch-1.0");
        cmd.args([
            "-q",
            "pipewiresrc",
            &format!("fd={raw_fd}"),
            &format!("path={node_id}"),
            "do-timestamp=true",
            "keepalive-time=500",
            "!",
            "videoconvert",
            "!",
            "videoscale",
            "method=bilinear",
            "!",
            &caps,
            "!",
            "fdsink",
            "fd=1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

        // Clear FD_CLOEXEC on the PipeWire remote FD in the child right before exec
        // so `gst-launch-1.0 pipewiresrc fd=<raw_fd>` inherits the portal FD.
        unsafe {
            cmd.pre_exec(move || {
                let flags = libc::fcntl(raw_fd, libc::F_GETFD);
                if flags < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(raw_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(err) => {
                fail(format!("Không chạy được gst-launch-1.0: {err}"));
                return;
            }
        };
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            fail("Không mở được luồng hình ảnh từ PipeWire.".into());
            return;
        };

        eprintln!(
            "zalo-gpui call: screen share Wayland Portal node={node_id} {src_w}x{src_h} -> {enc_w}x{enc_h} @{MIN_FPS}..{MAX_FPS}fps"
        );

        let frame_bytes = enc_w * enc_h * 3 / 2;
        let y_len = enc_w * enc_h;
        let mut buf = vec![0_u8; frame_bytes];
        let started_at = Instant::now();
        let mut last_key = started_at - KEY_INTERVAL;
        let mut pacer = AdaptivePacer::new(started_at);
        let mut frame_no = 0_u64;

        while !stop.load(Ordering::Acquire) {
            if stdout.read_exact(&mut buf).is_err() {
                if !stop.load(Ordering::Acquire) {
                    fail("Luồng chia sẻ màn hình Wayland đã kết thúc.".into());
                }
                break;
            }
            let now = Instant::now();
            let want_key =
                force_key.swap(false, Ordering::AcqRel) || last_key.elapsed() >= KEY_INTERVAL;
            let (should_encode, _next_interval) = pacer.evaluate(&buf[..y_len], want_key, now);
            if !should_encode {
                continue;
            }
            if frame_no % 2 == 0 {
                if let Some(bgra) = i420_to_bgra_preview(&buf, enc_w, enc_h, prev_w, prev_h) {
                    if let Ok(mut slot) = preview.lock() {
                        *slot = Some(LocalPreview {
                            w: prev_w as u32,
                            h: prev_h as u32,
                            bgra: Arc::new(bgra),
                        });
                    }
                }
            }
            frame_no = frame_no.wrapping_add(1);

            if want_key {
                encoder.force_intra_frame();
                last_key = now;
            }
            let picture = YUVBuffer::from_vec(buf.clone(), enc_w, enc_h);
            let ts_ms = now.duration_since(started_at).as_millis() as u64;
            let Ok(bitstream) = encoder.encode_at(&picture, Timestamp::from_millis(ts_ms)) else {
                continue;
            };
            if bitstream.frame_type() == openh264::encoder::FrameType::Skip {
                continue;
            }
            let data = bitstream.to_vec();
            if data.is_empty() {
                continue;
            }
            let key = bitstream.frame_type() == openh264::encoder::FrameType::IDR;
            if key {
                last_key = now;
            }
            match tx.try_send(CamEvent::Frame { key, data }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    if key {
                        force_key.store(true, Ordering::Release);
                    }
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        drop(session);
        return;
    }

    if tool_exists("grim") {
        capture_grim_loop(stop, force_key, tx, preview);
        return;
    }
    fail("Không tìm thấy XDG ScreenCast Portal hoặc grim trên phiên Wayland.".into());
}

/// Fallback Wayland screen capture via `grim -t ppm -` (wlroots: Sway/Hyprland).
fn capture_grim_loop(
    stop: &AtomicBool,
    force_key: &AtomicBool,
    tx: &SyncSender<CamEvent>,
    preview: &Mutex<Option<LocalPreview>>,
) {
    let fail = |text: String| {
        let _ = tx.try_send(CamEvent::Failed(text));
    };
    let mut encoder: Option<(openh264::encoder::Encoder, usize, usize)> = None;
    let started_at = Instant::now();
    let mut last_key = started_at - KEY_INTERVAL;
    let mut pacer = AdaptivePacer::new(started_at);
    let mut next_tick = Instant::now();
    let mut frame_no = 0_u64;

    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now < next_tick {
            std::thread::sleep((next_tick - now).min(Duration::from_millis(25)));
            continue;
        }
        let out = match Command::new("grim")
            .args(["-t", "ppm", "-"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            Ok(o) if o.status.success() => o.stdout,
            _ => {
                if !stop.load(Ordering::Acquire) {
                    fail("grim ngừng chụp màn hình Wayland.".into());
                }
                return;
            }
        };
        let Some((cap_w, cap_h, rgb)) = parse_ppm_p6(&out) else {
            continue;
        };
        let (want_w, want_h) = fit_screen_size(cap_w, cap_h);
        if encoder.as_ref().is_none_or(|(_, w, h)| (*w, *h) != (want_w, want_h)) {
            match open_screen_encoder(want_w, want_h) {
                Ok(enc) => {
                    encoder = Some((enc, want_w, want_h));
                    pacer.reset();
                    force_key.store(true, Ordering::Release);
                }
                Err(_) => {
                    fail("Không mở được bộ mã hình chia sẻ màn hình.".into());
                    return;
                }
            }
        }
        let Some((enc, enc_w, enc_h)) = encoder.as_mut() else {
            continue;
        };
        let (enc_w, enc_h) = (*enc_w, *enc_h);
        let enc_bgra = rgb_to_bgra_area(rgb, cap_w, cap_h, enc_w, enc_h);
        let i420 = bgra_to_i420(&enc_bgra, enc_w, enc_h);
        let now = Instant::now();
        let want_key =
            force_key.swap(false, Ordering::AcqRel) || last_key.elapsed() >= KEY_INTERVAL;
        let (should_encode, next_interval) = pacer.evaluate(&i420[..enc_w * enc_h], want_key, now);
        next_tick = (now + next_interval).max(now);
        if !should_encode {
            continue;
        }
        if frame_no % 2 == 0 {
            let (prev_w, prev_h) = fit_edge_even(enc_w, enc_h, PREVIEW_MAX_EDGE);
            let prev_bgra = if (prev_w, prev_h) == (enc_w, enc_h) {
                enc_bgra
            } else {
                scale_bgrx_area(&enc_bgra, enc_w, enc_h, 4, prev_w, prev_h)
            };
            if let Ok(mut slot) = preview.lock() {
                *slot = Some(LocalPreview {
                    w: prev_w as u32,
                    h: prev_h as u32,
                    bgra: Arc::new(prev_bgra),
                });
            }
        }
        frame_no = frame_no.wrapping_add(1);
        if want_key {
            enc.force_intra_frame();
            last_key = now;
        }
        let picture = YUVBuffer::from_vec(i420, enc_w, enc_h);
        let ts_ms = now.duration_since(started_at).as_millis() as u64;
        let Ok(bitstream) = enc.encode_at(&picture, Timestamp::from_millis(ts_ms)) else {
            continue;
        };
        if bitstream.frame_type() == openh264::encoder::FrameType::Skip {
            continue;
        }
        let data = bitstream.to_vec();
        if data.is_empty() {
            continue;
        }
        let key = bitstream.frame_type() == openh264::encoder::FrameType::IDR;
        if key {
            last_key = now;
        }
        match tx.try_send(CamEvent::Frame { key, data }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                if key {
                    force_key.store(true, Ordering::Release);
                }
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

/// Parses a binary PPM (`P6`) image into `(width, height, rgb_bytes)`.
pub(crate) fn parse_ppm_p6(bytes: &[u8]) -> Option<(usize, usize, &[u8])> {
    if !bytes.starts_with(b"P6") {
        return None;
    }
    let mut pos = 2usize;
    let mut nums = [0usize; 3];
    for slot in &mut nums {
        while pos < bytes.len() {
            let b = bytes[pos];
            if b == b'#' {
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
            } else if b.is_ascii_whitespace() {
                pos += 1;
            } else {
                break;
            }
        }
        let start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        if start == pos {
            return None;
        }
        *slot = std::str::from_utf8(&bytes[start..pos]).ok()?.parse().ok()?;
    }
    if pos >= bytes.len() || !bytes[pos].is_ascii_whitespace() {
        return None;
    }
    pos += 1;
    let (w, h, maxval) = (nums[0], nums[1], nums[2]);
    if w == 0 || h == 0 || maxval != 255 {
        return None;
    }
    let need = w.checked_mul(h)?.checked_mul(3)?;
    if bytes.len().saturating_sub(pos) < need {
        return None;
    }
    Some((w, h, &bytes[pos..pos + need]))
}

fn rgb_to_bgra_area(rgb: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut bgr = vec![0_u8; sw * sh * 3];
    for (dst, src) in bgr.chunks_exact_mut(3).zip(rgb.chunks_exact(3)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
    }
    scale_bgrx_area(&bgr, sw, sh, 3, dw, dh)
}

fn i420_to_bgra_preview(
    i420: &[u8],
    w: usize,
    h: usize,
    prev_w: usize,
    prev_h: usize,
) -> Option<Vec<u8>> {
    let y_len = w.checked_mul(h)?;
    let c_len = (w / 2).checked_mul(h / 2)?;
    if w % 2 != 0 || h % 2 != 0 || i420.len() < y_len + c_len * 2 {
        return None;
    }
    let y = &i420[..y_len];
    let u = &i420[y_len..y_len + c_len];
    let v = &i420[y_len + c_len..y_len + c_len * 2];
    let cw = w / 2;
    let mut bgra = vec![255_u8; y_len * 4];
    for row in 0..h {
        let u_row = (row / 2) * cw;
        for col in 0..w {
            let c = i32::from(y[row * w + col]) - 16;
            let d = i32::from(u[u_row + col / 2]) - 128;
            let e = i32::from(v[u_row + col / 2]) - 128;
            let y298 = 298 * c + 128;
            let r = ((y298 + 409 * e) >> 8).clamp(0, 255) as u8;
            let g = ((y298 - 100 * d - 208 * e) >> 8).clamp(0, 255) as u8;
            let b = ((y298 + 516 * d) >> 8).clamp(0, 255) as u8;
            let px = (row * w + col) * 4;
            bgra[px] = b;
            bgra[px + 1] = g;
            bgra[px + 2] = r;
        }
    }
    if (prev_w, prev_h) == (w, h) {
        Some(bgra)
    } else {
        Some(scale_bgrx_area(&bgra, w, h, 4, prev_w, prev_h))
    }
}

fn resolve_grab_rect(
    conn: &RustConnection,
    root: Window,
    root_w: u16,
    root_h: u16,
    target: &ShareTarget,
) -> Option<(i16, i16, u16, u16)> {
    match target {
        ShareTarget::WaylandPortal | ShareTarget::FullScreen => {
            if let Some(primary) = list_randr_monitors(conn, root).into_iter().next() {
                Some((primary.x, primary.y, primary.w, primary.h))
            } else {
                Some((0, 0, root_w, root_h))
            }
        }
        ShareTarget::Region { x, y, w, h, .. } => {
            let x0 = (*x).clamp(0, root_w as i16);
            let y0 = (*y).clamp(0, root_h as i16);
            let max_w = root_w.saturating_sub(x0 as u16);
            let max_h = root_h.saturating_sub(y0 as u16);
            let cw = (*w).min(max_w);
            let ch = (*h).min(max_h);
            (cw >= 16 && ch >= 16).then_some((x0, y0, cw, ch))
        }
        ShareTarget::Window { id, .. } => {
            let geom = conn.get_geometry(*id).ok()?.reply().ok()?;
            let tr = conn.translate_coordinates(*id, root, 0, 0).ok()?.reply().ok()?;
            let x0 = tr.dst_x.max(0);
            let y0 = tr.dst_y.max(0);
            let x1 = (i32::from(tr.dst_x) + i32::from(geom.width)).min(i32::from(root_w));
            let y1 = (i32::from(tr.dst_y) + i32::from(geom.height)).min(i32::from(root_h));
            let cw = (x1 - i32::from(x0)).max(0) as u16;
            let ch = (y1 - i32::from(y0)).max(0) as u16;
            (cw >= 16 && ch >= 16).then_some((x0, y0, cw, ch))
        }
    }
}

fn capture_loop_inner(
    stop: &AtomicBool,
    force_key: &AtomicBool,
    tx: &SyncSender<CamEvent>,
    preview: &Mutex<Option<LocalPreview>>,
    target: &ShareTarget,
) {
    let fail = |text: String| {
        let _ = tx.try_send(CamEvent::Failed(text));
    };
    let (conn, screen_num) = match RustConnection::connect(None) {
        Ok(pair) => pair,
        Err(err) => {
            fail(format!("Không kết nối được X11: {err}"));
            return;
        }
    };
    let Some(screen) = conn.setup().roots.get(screen_num) else {
        fail("Không tìm thấy màn hình X11.".into());
        return;
    };
    let root = screen.root;
    let root_w = screen.width_in_pixels;
    let root_h = screen.height_in_pixels;
    let Some((_, _, init_w, init_h)) = resolve_grab_rect(&conn, root, root_w, root_h, target)
    else {
        fail("Không đọc được vùng/cửa sổ chia sẻ màn hình.".into());
        return;
    };
    let (mut enc_w, mut enc_h) = fit_screen_size(usize::from(init_w), usize::from(init_h));
    let mut encoder = match open_screen_encoder(enc_w, enc_h) {
        Ok(enc) => enc,
        Err(_) => {
            fail("Không mở được bộ mã hình chia sẻ màn hình.".into());
            return;
        }
    };
    eprintln!(
        "zalo-gpui call: screen share X11 «{}» {}x{} -> {}x{} @{}..{}fps",
        target.label(),
        init_w,
        init_h,
        enc_w,
        enc_h,
        MIN_FPS,
        MAX_FPS
    );

    let started_at = Instant::now();
    let mut last_key = started_at - KEY_INTERVAL;
    let mut pacer = AdaptivePacer::new(started_at);
    let mut frame_no = 0_u64;
    let mut next_tick = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now < next_tick {
            let sleep_for = (next_tick - now).min(Duration::from_millis(20));
            std::thread::sleep(sleep_for);
            continue;
        }

        let Some((gx, gy, gw, gh)) = resolve_grab_rect(&conn, root, root_w, root_h, target) else {
            if !stop.load(Ordering::Acquire) {
                fail("Cửa sổ đang chia sẻ đã đóng hoặc thu nhỏ.".into());
            }
            return;
        };
        let cap_w = usize::from(gw);
        let cap_h = usize::from(gh);
        let (want_w, want_h) = fit_screen_size(cap_w, cap_h);
        if (want_w, want_h) != (enc_w, enc_h) {
            if let Ok(next_enc) = open_screen_encoder(want_w, want_h) {
                encoder = next_enc;
                enc_w = want_w;
                enc_h = want_h;
                pacer.reset();
                force_key.store(true, Ordering::Release);
            }
        }
        let (prev_w, prev_h) = fit_edge_even(enc_w, enc_h, PREVIEW_MAX_EDGE);

        let reply = match conn
            .get_image(ImageFormat::Z_PIXMAP, root, gx, gy, gw, gh, !0)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        {
            Some(reply) => reply,
            None => {
                if !stop.load(Ordering::Acquire) {
                    fail("Màn hình X11 ngừng trả hình.".into());
                }
                return;
            }
        };
        let bpp = reply.data.len() / (cap_w * cap_h).max(1);
        if bpp < 3 || reply.data.len() < cap_w * cap_h * bpp {
            continue;
        }

        // Single high-quality area-box downscale to encode size, then derive
        // both I420 (with 2×2 chroma averaging) and crisp local preview.
        let enc_bgra = scale_bgrx_area(&reply.data, cap_w, cap_h, bpp, enc_w, enc_h);
        let i420 = bgra_to_i420(&enc_bgra, enc_w, enc_h);
        let now = Instant::now();
        let want_key =
            force_key.swap(false, Ordering::AcqRel) || last_key.elapsed() >= KEY_INTERVAL;
        let (should_encode, next_interval) = pacer.evaluate(&i420[..enc_w * enc_h], want_key, now);
        next_tick = (now + next_interval).max(now);
        if !should_encode {
            continue;
        }

        if frame_no % 2 == 0 {
            let prev_bgra = if (prev_w, prev_h) == (enc_w, enc_h) {
                enc_bgra
            } else {
                scale_bgrx_area(&enc_bgra, enc_w, enc_h, 4, prev_w, prev_h)
            };
            if let Ok(mut slot) = preview.lock() {
                *slot = Some(LocalPreview {
                    w: prev_w as u32,
                    h: prev_h as u32,
                    bgra: Arc::new(prev_bgra),
                });
            }
        }
        frame_no = frame_no.wrapping_add(1);

        if want_key {
            encoder.force_intra_frame();
            last_key = now;
        }
        let picture = YUVBuffer::from_vec(i420, enc_w, enc_h);
        let ts_ms = now.duration_since(started_at).as_millis() as u64;
        let Ok(bitstream) = encoder.encode_at(&picture, Timestamp::from_millis(ts_ms)) else {
            continue;
        };
        if bitstream.frame_type() == openh264::encoder::FrameType::Skip {
            continue;
        }
        let data = bitstream.to_vec();
        if data.is_empty() {
            continue;
        }
        let key = bitstream.frame_type() == openh264::encoder::FrameType::IDR;
        if key {
            last_key = Instant::now();
        }
        match tx.try_send(CamEvent::Frame { key, data }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                if key {
                    force_key.store(true, Ordering::Release);
                }
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

/// Even aspect-preserving dimensions fitting inside `MAX_WIDTH × MAX_HEIGHT`.
pub(crate) fn fit_screen_size(cap_w: usize, cap_h: usize) -> (usize, usize) {
    if cap_w == 0 || cap_h == 0 {
        return (MAX_WIDTH, MAX_HEIGHT);
    }
    let scale = (MAX_WIDTH as f64 / cap_w as f64)
        .min(MAX_HEIGHT as f64 / cap_h as f64)
        .min(1.0);
    let w = (((cap_w as f64 * scale).round() as usize).max(16)) & !1;
    let h = (((cap_h as f64 * scale).round() as usize).max(16)) & !1;
    (w, h)
}

fn fit_edge_even(w: usize, h: usize, max_edge: usize) -> (usize, usize) {
    let long = w.max(h);
    if long <= max_edge || long == 0 {
        return ((w.max(2)) & !1, (h.max(2)) & !1);
    }
    let scale = max_edge as f64 / long as f64;
    let nw = (((w as f64 * scale).round() as usize).max(2)) & !1;
    let nh = (((h as f64 * scale).round() as usize).max(2)) & !1;
    (nw, nh)
}

#[derive(Clone, Copy)]
struct AreaSpan {
    start: usize,
    end: usize,
    /// Weight of the first source pixel in `[1..=256]`.
    w_first: u32,
    /// Weight of the last source pixel in `[1..=256]` (unused when `start == end`).
    w_last: u32,
    /// Sum of all weights for this destination pixel.
    w_sum: u32,
}

fn build_area_spans(src_len: usize, dst_len: usize) -> Vec<AreaSpan> {
    let mut spans = Vec::with_capacity(dst_len);
    let scale = src_len as f64 / dst_len as f64;
    for d in 0..dst_len {
        let s0 = d as f64 * scale;
        let s1 = ((d + 1) as f64 * scale).min(src_len as f64);
        let start = (s0.floor() as usize).min(src_len - 1);
        let end = ((s1.ceil() as usize).saturating_sub(1)).clamp(start, src_len - 1);
        if start == end {
            spans.push(AreaSpan {
                start,
                end,
                w_first: 256,
                w_last: 0,
                w_sum: 256,
            });
        } else {
            let wf = (((start + 1) as f64 - s0).clamp(0.0, 1.0) * 256.0).round() as u32;
            let wl = ((s1 - end as f64).clamp(0.0, 1.0) * 256.0).round() as u32;
            let mid = end.saturating_sub(start + 1) as u32;
            let wf = wf.max(1);
            let wl = wl.max(1);
            let w_sum = wf + wl + mid * 256;
            spans.push(AreaSpan {
                start,
                end,
                w_first: wf,
                w_last: wl,
                w_sum,
            });
        }
    }
    spans
}

/// 2-pass separable box-area (`INTER_AREA`) downscaler for X11 `BGRX`/`BGR`
/// buffers into tightly packed BGRA `[B, G, R, 255]`.
///
/// Unlike nearest-neighbor sampling, every 1px desktop glyph stroke contributes
/// proportionally to the output pixel so small UI text remains crisp and legible.
pub(crate) fn scale_bgrx_area(
    src: &[u8],
    sw: usize,
    sh: usize,
    bpp: usize,
    dw: usize,
    dh: usize,
) -> Vec<u8> {
    let mut dst = vec![255_u8; dw * dh * 4];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || bpp < 3 || src.len() < sw * sh * bpp {
        return dst;
    }
    // Exact 1:1 fast path: zero blur, direct copy.
    if sw == dw && sh == dh {
        if bpp == 4 {
            dst.copy_from_slice(&src[..dw * dh * 4]);
            for px in dst.chunks_exact_mut(4) {
                px[3] = 255;
            }
        } else {
            for i in 0..(dw * dh) {
                let s = i * bpp;
                let d = i * 4;
                dst[d] = src[s];
                dst[d + 1] = src[s + 1];
                dst[d + 2] = src[s + 2];
                dst[d + 3] = 255;
            }
        }
        return dst;
    }

    let x_spans = build_area_spans(sw, dw);
    let y_spans = build_area_spans(sh, dh);
    let src_row_stride = sw * bpp;

    // Pass 1: horizontal area filter (sw × sh -> dw × sh, packed BGR 3 bytes/px).
    let mut horiz = vec![0_u8; dw * sh * 3];
    for sy in 0..sh {
        let srow = sy * src_row_stride;
        let hrow = sy * dw * 3;
        for (dx, span) in x_spans.iter().enumerate() {
            let hd = hrow + dx * 3;
            if span.start == span.end {
                let p = srow + span.start * bpp;
                horiz[hd] = src[p];
                horiz[hd + 1] = src[p + 1];
                horiz[hd + 2] = src[p + 2];
                continue;
            }
            let p0 = srow + span.start * bpp;
            let mut sb = u32::from(src[p0]) * span.w_first;
            let mut sg = u32::from(src[p0 + 1]) * span.w_first;
            let mut sr = u32::from(src[p0 + 2]) * span.w_first;
            for sx in (span.start + 1)..span.end {
                let p = srow + sx * bpp;
                sb += u32::from(src[p]) << 8;
                sg += u32::from(src[p + 1]) << 8;
                sr += u32::from(src[p + 2]) << 8;
            }
            let p1 = srow + span.end * bpp;
            sb += u32::from(src[p1]) * span.w_last;
            sg += u32::from(src[p1 + 1]) * span.w_last;
            sr += u32::from(src[p1 + 2]) * span.w_last;
            let half = span.w_sum >> 1;
            horiz[hd] = ((sb + half) / span.w_sum) as u8;
            horiz[hd + 1] = ((sg + half) / span.w_sum) as u8;
            horiz[hd + 2] = ((sr + half) / span.w_sum) as u8;
        }
    }

    // Pass 2: vertical area filter (dw × sh -> dw × dh, packed BGRA 4 bytes/px).
    let h_row_stride = dw * 3;
    let mut acc = vec![0_u32; h_row_stride];
    for (dy, span) in y_spans.iter().enumerate() {
        let drow = dy * dw * 4;
        if span.start == span.end {
            let hrow = span.start * h_row_stride;
            for dx in 0..dw {
                let s = hrow + dx * 3;
                let d = drow + dx * 4;
                dst[d] = horiz[s];
                dst[d + 1] = horiz[s + 1];
                dst[d + 2] = horiz[s + 2];
                dst[d + 3] = 255;
            }
            continue;
        }
        acc.fill(0);
        let r0 = &horiz[span.start * h_row_stride..(span.start + 1) * h_row_stride];
        for (a, &v) in acc.iter_mut().zip(r0.iter()) {
            *a += u32::from(v) * span.w_first;
        }
        for sy in (span.start + 1)..span.end {
            let rm = &horiz[sy * h_row_stride..(sy + 1) * h_row_stride];
            for (a, &v) in acc.iter_mut().zip(rm.iter()) {
                *a += u32::from(v) << 8;
            }
        }
        let r1 = &horiz[span.end * h_row_stride..(span.end + 1) * h_row_stride];
        for (a, &v) in acc.iter_mut().zip(r1.iter()) {
            *a += u32::from(v) * span.w_last;
        }
        let half = span.w_sum >> 1;
        for dx in 0..dw {
            let a = dx * 3;
            let d = drow + dx * 4;
            dst[d] = ((acc[a] + half) / span.w_sum) as u8;
            dst[d + 1] = ((acc[a + 1] + half) / span.w_sum) as u8;
            dst[d + 2] = ((acc[a + 2] + half) / span.w_sum) as u8;
            dst[d + 3] = 255;
        }
    }

    dst
}

/// Converts tightly packed BGRA `[B, G, R, A]` into packed `I420` (`yuv420p`)
/// using BT.601 studio swing and proper 2×2 chroma block averaging.
pub(crate) fn bgra_to_i420(bgra: &[u8], w: usize, h: usize) -> Vec<u8> {
    let y_len = w * h;
    let cw = w / 2;
    let ch = h / 2;
    let c_len = cw * ch;
    let mut out = vec![0_u8; y_len + c_len * 2];
    if w < 2 || h < 2 || bgra.len() < w * h * 4 {
        return out;
    }
    let (y_plane, rest) = out.split_at_mut(y_len);
    let (u_plane, v_plane) = rest.split_at_mut(c_len);
    let row_bytes = w * 4;

    for cy in 0..ch {
        let y0 = cy * 2;
        let y1 = y0 + 1;
        let srow0 = y0 * row_bytes;
        let srow1 = y1 * row_bytes;
        let yrow0 = y0 * w;
        let yrow1 = y1 * w;
        let crow = cy * cw;
        for cx in 0..cw {
            let x0 = cx * 2;
            let x1 = x0 + 1;
            let p00 = srow0 + x0 * 4;
            let p01 = srow0 + x1 * 4;
            let p10 = srow1 + x0 * 4;
            let p11 = srow1 + x1 * 4;

            let b00 = i32::from(bgra[p00]);
            let g00 = i32::from(bgra[p00 + 1]);
            let r00 = i32::from(bgra[p00 + 2]);

            let b01 = i32::from(bgra[p01]);
            let g01 = i32::from(bgra[p01 + 1]);
            let r01 = i32::from(bgra[p01 + 2]);

            let b10 = i32::from(bgra[p10]);
            let g10 = i32::from(bgra[p10 + 1]);
            let r10 = i32::from(bgra[p10 + 2]);

            let b11 = i32::from(bgra[p11]);
            let g11 = i32::from(bgra[p11 + 1]);
            let r11 = i32::from(bgra[p11 + 2]);

            y_plane[yrow0 + x0] =
                (((66 * r00 + 129 * g00 + 25 * b00 + 128) >> 8) + 16).clamp(0, 255) as u8;
            y_plane[yrow0 + x1] =
                (((66 * r01 + 129 * g01 + 25 * b01 + 128) >> 8) + 16).clamp(0, 255) as u8;
            y_plane[yrow1 + x0] =
                (((66 * r10 + 129 * g10 + 25 * b10 + 128) >> 8) + 16).clamp(0, 255) as u8;
            y_plane[yrow1 + x1] =
                (((66 * r11 + 129 * g11 + 25 * b11 + 128) >> 8) + 16).clamp(0, 255) as u8;

            let r_avg = (r00 + r01 + r10 + r11 + 2) >> 2;
            let g_avg = (g00 + g01 + g10 + g11 + 2) >> 2;
            let b_avg = (b00 + b01 + b10 + b11 + 2) >> 2;
            let c_idx = crow + cx;
            u_plane[c_idx] =
                (((-38 * r_avg - 74 * g_avg + 112 * b_avg + 128) >> 8) + 128).clamp(0, 255) as u8;
            v_plane[c_idx] =
                (((112 * r_avg - 94 * g_avg - 18 * b_avg + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
    out
}

/// Scales X11 `BGRX`/`BGR` with box-area filtering and converts to packed `I420`.
#[cfg(test)]
pub(crate) fn bgrx_to_scaled_i420(
    src: &[u8],
    sw: usize,
    sh: usize,
    bpp: usize,
    dw: usize,
    dh: usize,
) -> Vec<u8> {
    let bgra = scale_bgrx_area(src, sw, sh, bpp, dw, dh);
    bgra_to_i420(&bgra, dw, dh)
}

fn open_screen_encoder(
    width: usize,
    height: usize,
) -> Result<openh264::encoder::Encoder, openh264::Error> {
    use openh264::encoder::{
        BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Level, Profile,
        QpRange, RateControlMode, UsageType,
    };
    use openh264::OpenH264API;
    let api = OpenH264API::from_source();
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(BITRATE_BPS))
        .max_frame_rate(FrameRate::from_hz(MAX_FPS as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(0))
        .profile(Profile::Baseline)
        .level(Level::Level_4_0)
        .complexity(Complexity::Medium)
        .qp(QpRange::new(12, 34))
        .adaptive_quantization(false)
        .background_detection(false)
        .long_term_reference(false)
        .skip_frames(true)
        .num_threads(if width * height > 640 * 360 { 2 } else { 1 });
    Encoder::with_api_config(api, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_common_desktop_resolutions_into_720p_even_bounds() {
        assert_eq!(fit_screen_size(1920, 1080), (1280, 720));
        assert_eq!(fit_screen_size(2560, 1440), (1280, 720));
        assert_eq!(fit_screen_size(1366, 768), (1280, 720));
        assert_eq!(fit_screen_size(1024, 768), (960, 720));
        assert_eq!(fit_screen_size(800, 600), (800, 600));
    }

    #[test]
    fn area_downscale_preserves_thin_1px_strokes() {
        // 6×6 black image with a 1px white vertical line at x = 1 and horizontal line at y = 1.
        // Nearest-neighbor 6->2 would drop x=1/y=1 completely; area filter keeps non-zero energy.
        let mut src = vec![0_u8; 6 * 6 * 4];
        for y in 0..6 {
            for x in 0..6 {
                let p = (y * 6 + x) * 4;
                if x == 1 || y == 1 {
                    src[p] = 255;
                    src[p + 1] = 255;
                    src[p + 2] = 255;
                }
                src[p + 3] = 255;
            }
        }
        let dst = scale_bgrx_area(&src, 6, 6, 4, 2, 2);
        // Top-left output pixel covers [0..3, 0..3] (5 white out of 9 pixels -> ~142).
        assert!(dst[0] >= 120 && dst[0] <= 165, "expected ~142, got {}", dst[0]);
        assert_eq!(dst[3], 255);
    }

    #[test]
    fn converts_bgrx_to_i420_and_encodes_annex_b_keyframe() {
        let mut src = vec![0_u8; 64 * 36 * 4];
        for px in src.chunks_exact_mut(4) {
            px.copy_from_slice(&[240, 120, 40, 255]);
        }
        let i420 = bgrx_to_scaled_i420(&src, 64, 36, 4, 32, 18);
        assert_eq!(i420.len(), 32 * 18 * 3 / 2);
        let mut enc = open_screen_encoder(32, 18).expect("screen encoder");
        enc.force_intra_frame();
        let pic = YUVBuffer::from_vec(i420, 32, 18);
        let bs = enc
            .encode_at(&pic, Timestamp::from_millis(0))
            .expect("encode screen frame");
        assert!(!bs.to_vec().is_empty());
        assert_eq!(bs.frame_type(), openh264::encoder::FrameType::IDR);
    }

    #[test]
    fn adaptive_pacer_ramps_up_on_motion_and_skips_static_frames() {
        let t0 = Instant::now();
        let mut pacer = AdaptivePacer::new(t0);
        let frame_a = vec![40_u8; 640 * 360];
        let mut frame_b = frame_a.clone();
        for b in frame_b.iter_mut().take(16_000) {
            *b = 200;
        }
        // First frame always encodes at MAX_FPS cadence.
        let (send, interval) = pacer.evaluate(&frame_a, false, t0);
        assert!(send);
        assert!(interval <= Duration::from_millis(75));

        // Identical frame shortly after: skipped during static period.
        let (send, _) = pacer.evaluate(&frame_a, false, t0 + Duration::from_millis(100));
        assert!(!send);

        // After MOTION_HOLD without change: drops cadence to MIN_FPS (250 ms) and sends keepalive.
        let (send, interval) = pacer.evaluate(&frame_a, false, t0 + Duration::from_millis(1_700));
        assert!(send);
        assert!(interval >= Duration::from_millis(200));

        // New motion immediately ramps back up to MAX_FPS.
        let (send, interval) = pacer.evaluate(&frame_b, false, t0 + Duration::from_millis(1_800));
        assert!(send);
        assert!(interval <= Duration::from_millis(75));
    }

    #[test]
    fn parses_binary_ppm_p6_with_comments() {
        let mut ppm = b"P6\n# grim capture\n2 2\n255\n".to_vec();
        ppm.extend_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]);
        let (w, h, rgb) = parse_ppm_p6(&ppm).expect("ppm");
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgb.len(), 12);
        assert_eq!(&rgb[0..3], &[10, 20, 30]);
    }
}
