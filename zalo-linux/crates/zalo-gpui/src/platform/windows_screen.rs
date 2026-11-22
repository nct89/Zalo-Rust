//! Native Windows screen sharing via Win32 GDI (`EnumDisplayMonitors` + `BitBlt`),
//! encoded with OpenH264. Requires zero external binaries (`ffmpeg` is not needed).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile,
    RateControlMode, UsageType,
};
use openh264::formats::YUVBuffer;
use openh264::{OpenH264API, Timestamp};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, HMONITOR, MONITORINFO, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::call_camera::{CamEvent, LocalPreview};
use crate::windows_media::{bgra_to_i420, downscale_bgra};

const ENCODE_W: usize = 1280;
const ENCODE_H: usize = 720;
const TARGET_FPS: u64 = 12;

/// A screen or display the user can share.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShareTarget {
    /// Primary display (or full virtual desktop if single monitor).
    #[default]
    FullScreen,
    /// Specific display bounds in virtual desktop pixels.
    Region {
        /// Row label.
        label: String,
        /// Left edge.
        x: i32,
        /// Top edge.
        y: i32,
        /// Width.
        w: u32,
        /// Height.
        h: u32,
    },
}

impl ShareTarget {
    /// Picker title.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::FullScreen => "Toàn màn hình".into(),
            Self::Region { label, .. } => label.clone(),
        }
    }

    /// Secondary line under the title.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::FullScreen => "Màn hình chính hiện tại".into(),
            Self::Region { w, h, .. } if *w > 0 && *h > 0 => format!("Màn hình • {w}×{h}"),
            Self::Region { .. } => "Màn hình".into(),
        }
    }

    fn rect(&self) -> (i32, i32, i32, i32) {
        match *self {
            Self::FullScreen => unsafe {
                let w = GetSystemMetrics(SM_CXSCREEN);
                let h = GetSystemMetrics(SM_CYSCREEN);
                if w > 0 && h > 0 {
                    (0, 0, w, h)
                } else {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN).max(640),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN).max(480),
                    )
                }
            },
            Self::Region { x, y, w, h, .. } => (x, y, (w as i32).max(2), (h as i32).max(2)),
        }
    }
}

/// Desktop capture encoded on a background thread.
pub(crate) struct ScreenCapture {
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<CamEvent>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
}

impl ScreenCapture {
    /// Starts native Win32 GDI screen capture.
    ///
    /// # Errors
    ///
    /// Returns when the worker thread cannot start.
    pub(crate) fn start(target: ShareTarget) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let force_key = Arc::new(AtomicBool::new(true));
        let preview = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::sync_channel(3);
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

    /// Requests an IDR on the next frame.
    pub(crate) fn request_key_frame(&self) {
        self.force_key.store(true, Ordering::Release);
    }

    /// Latest local preview.
    pub(crate) fn take_preview(&self) -> Option<LocalPreview> {
        self.preview.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Next encoded frame.
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

/// Native Win32 GDI capture is always available on Windows.
#[must_use]
pub(crate) fn grab_unavailable() -> Option<String> {
    None
}

/// Enumerates physical monitors via `EnumDisplayMonitors`.
#[must_use]
pub(crate) fn list_share_targets() -> Vec<ShareTarget> {
    let monitors = enum_monitors();
    if monitors.is_empty() {
        return vec![ShareTarget::FullScreen];
    }
    monitors
        .into_iter()
        .enumerate()
        .map(|(idx, (rect, primary))| {
            let w = (rect.right - rect.left).max(0) as u32;
            let h = (rect.bottom - rect.top).max(0) as u32;
            let label = if primary {
                format!("Màn hình {} (Chính)", idx + 1)
            } else {
                format!("Màn hình {}", idx + 1)
            };
            ShareTarget::Region {
                label,
                x: rect.left,
                y: rect.top,
                w,
                h,
            }
        })
        .collect()
}

fn enum_monitors() -> Vec<(RECT, bool)> {
    unsafe extern "system" fn callback(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let out = &mut *(data.0 as *mut Vec<(RECT, bool)>);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut info).as_bool() {
            let is_primary = (info.dwFlags & 1) != 0;
            out.push((info.rcMonitor, is_primary));
        }
        BOOL(1)
    }

    let mut out: Vec<(RECT, bool)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM((&mut out as *mut Vec<(RECT, bool)>) as isize),
        );
    }
    out.sort_by_key(|(_, primary)| !*primary);
    out
}

fn capture_loop(
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    tx: mpsc::SyncSender<CamEvent>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
    target: ShareTarget,
) {
    let fail = |text: String| {
        let _ = tx.try_send(CamEvent::Failed(text));
    };
    let Ok(mut encoder) = open_encoder() else {
        fail("Không khởi tạo được bộ mã hóa H.264 cho màn hình.".into());
        return;
    };

    let (sx, sy, sw, sh) = target.rect();
    let frame_interval = Duration::from_millis(1000 / TARGET_FPS);
    let started = Instant::now();
    let mut frame_no = 0_u32;

    while !stop.load(Ordering::Acquire) {
        let tick = Instant::now();
        let Some(raw) = grab_rect_bgra(sx, sy, sw, sh) else {
            fail("Không chụp được màn hình Windows (GDI BitBlt thất bại).".into());
            return;
        };
        let scaled = downscale_bgra(&raw, sw as usize, sh as usize, ENCODE_W, ENCODE_H);
        if scaled.len() != ENCODE_W * ENCODE_H * 4 {
            continue;
        }
        if frame_no % 2 == 0 {
            let (pw, ph) = (960_usize, 540_usize);
            let small = downscale_bgra(&scaled, ENCODE_W, ENCODE_H, pw, ph);
            if let Ok(mut slot) = preview.lock() {
                *slot = Some(LocalPreview {
                    w: pw as u32,
                    h: ph as u32,
                    bgra: Arc::new(small),
                });
            }
        }
        frame_no = frame_no.wrapping_add(1);

        if force_key.swap(false, Ordering::AcqRel) {
            encoder.force_intra_frame();
        }
        let i420 = bgra_to_i420(&scaled, ENCODE_W, ENCODE_H);
        let picture = YUVBuffer::from_vec(i420, ENCODE_W, ENCODE_H);
        let ts = Timestamp::from_millis(started.elapsed().as_millis() as u64);
        if let Ok(bitstream) = encoder.encode_at(&picture, ts) {
            if bitstream.frame_type() != FrameType::Skip {
                let data = bitstream.to_vec();
                if !data.is_empty() {
                    let key = bitstream.frame_type() == FrameType::IDR;
                    if let Err(TrySendError::Disconnected(_)) =
                        tx.try_send(CamEvent::Frame { key, data })
                    {
                        return;
                    }
                }
            }
        }

        let elapsed = tick.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }
}

fn grab_rect_bgra(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let bmp = CreateCompatibleBitmap(screen_dc, w, h);
        let old = SelectObject(mem_dc, bmp.into());
        let ok = BitBlt(mem_dc, 0, 0, w, h, Some(screen_dc), x, y, SRCCOPY).is_ok();

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![255_u8; (w as usize) * (h as usize) * 4];
        let lines = if ok {
            GetDIBits(
                mem_dc,
                bmp,
                0,
                h as u32,
                Some(bgra.as_mut_ptr() as *mut c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(mem_dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);

        if lines == 0 {
            return None;
        }
        for px in bgra.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Some(bgra)
    }
}

fn open_encoder() -> Result<Encoder, openh264::Error> {
    let api = OpenH264API::from_source();
    let config = EncoderConfig::new()
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(1_400_000))
        .max_frame_rate(FrameRate::from_hz(TARGET_FPS as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(0))
        .profile(Profile::Baseline)
        .num_threads(2);
    Encoder::with_api_config(api, config)
}
