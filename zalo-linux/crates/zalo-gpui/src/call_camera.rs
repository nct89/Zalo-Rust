//! Local camera for a Zalo call.
//!
//! Captures MJPEG 720p (with safe fallback to standard v4l2 sizes) via ffmpeg,
//! crops cleanly to fill 16:9 without black bars, scales to the dynamic
//! `SendRate` rung (`Profile`) in pure Rust without restarting the webcam, and
//! encodes Baseline H.264 Annex-B via OpenH264 with PLI-triggered keyframes.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Level,
    Profile, QpRange, RateControlMode, SpsPpsStrategy, UsageType,
};
use openh264::formats::YUVBuffer;
use openh264::{OpenH264API, Timestamp};
use zcall_core::send_rate::Profile as SendProfile;

/// Repeat the last frame after a capture stall (`call.js` REPEAT_AFTER_MS).
const STALL_REPEAT_AFTER: Duration = Duration::from_millis(600);
/// Cadence while repeating a stalled camera frame (~4 fps).
const STALL_REPEAT_STEP: Duration = Duration::from_millis(250);

/// One encoded camera access unit, or why capture stopped.
pub(crate) enum CamEvent {
    /// Annex-B. `key` is an IDR (or SPS/PPS) the phone can start from.
    Frame {
        /// This access unit is a key frame.
        key: bool,
        /// Annex-B bytes.
        data: Vec<u8>,
    },
    /// Capture cannot continue (error message explaining why capture failed).
    Failed(String),
}

enum CamCtrl {
    Profile(SendProfile),
    ForceKey,
}

#[derive(Clone, Copy, Debug)]
struct AttemptSpec {
    cap_w: usize,
    cap_h: usize,
    input_size: Option<&'static str>,
    input_format: Option<&'static str>,
    fps: Option<u32>,
    timeout: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EncoderKey {
    width: usize,
    height: usize,
    bitrate_bps: u32,
    fps_rounded: u32,
}

/// OpenH264 C API option IDs (`openh264-sys2` `ENCODER_OPTION`).
const ENCODER_OPTION_FRAME_RATE: i32 = 4;
const ENCODER_OPTION_BITRATE: i32 = 5;
const ENCODER_OPTION_MAX_BITRATE: i32 = 6;
const SPATIAL_LAYER_0: std::os::raw::c_int = 0;
const SPATIAL_LAYER_ALL: std::os::raw::c_int = 4;

/// C ABI layout matching OpenH264 `SBitrateInfo` (`TagBitrateInfo`).
#[repr(C)]
struct OpenH264BitrateInfo {
    layer: std::os::raw::c_int,
    bitrate: std::os::raw::c_int,
}

/// Minimum interval between local self-view BGRA preview updates (~15 fps).
const LOCAL_PREVIEW_INTERVAL: Duration = Duration::from_millis(66);

fn needs_encoder_reopen(cur: Option<EncoderKey>, want: EncoderKey) -> bool {
    match cur {
        Some(prev) => prev.width != want.width || prev.height != want.height,
        None => true,
    }
}

/// Updates OpenH264 target bitrate, max bitrate, and max frame rate live via
/// `SetOption` without destroying encoder reference state or forcing an IDR.
fn update_encoder_rate(enc: &mut Encoder, bitrate_bps: u32, fps: u32) -> bool {
    let target_bps = bitrate_bps.clamp(80_000, 4_000_000) as std::os::raw::c_int;
    let max_bps = (bitrate_bps.saturating_mul(5) / 4).clamp(100_000, 5_000_000) as std::os::raw::c_int;
    let mut fps_val = fps.clamp(5, 30) as f32;
    let mut max_all = OpenH264BitrateInfo {
        layer: SPATIAL_LAYER_ALL,
        bitrate: max_bps,
    };
    let mut max_l0 = OpenH264BitrateInfo {
        layer: SPATIAL_LAYER_0,
        bitrate: max_bps,
    };
    let mut tgt_all = OpenH264BitrateInfo {
        layer: SPATIAL_LAYER_ALL,
        bitrate: target_bps,
    };
    let mut tgt_l0 = OpenH264BitrateInfo {
        layer: SPATIAL_LAYER_0,
        bitrate: target_bps,
    };
    unsafe {
        let raw = enc.raw_api();
        let ok_max_all = raw.set_option(
            ENCODER_OPTION_MAX_BITRATE,
            std::ptr::addr_of_mut!(max_all).cast(),
        ) == 0;
        let _ = raw.set_option(
            ENCODER_OPTION_MAX_BITRATE,
            std::ptr::addr_of_mut!(max_l0).cast(),
        );
        let ok_tgt_all = raw.set_option(
            ENCODER_OPTION_BITRATE,
            std::ptr::addr_of_mut!(tgt_all).cast(),
        ) == 0;
        let _ = raw.set_option(
            ENCODER_OPTION_BITRATE,
            std::ptr::addr_of_mut!(tgt_l0).cast(),
        );
        let ok_fps = raw.set_option(
            ENCODER_OPTION_FRAME_RATE,
            std::ptr::addr_of_mut!(fps_val).cast(),
        ) == 0;
        ok_max_all && ok_tgt_all && ok_fps
    }
}

struct TxStats {
    window_start: Instant,
    captured: u32,
    encoded: u32,
    skipped: u32,
    dropped: u32,
    keys: u32,
    bytes: usize,
}

impl TxStats {
    fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            captured: 0,
            encoded: 0,
            skipped: 0,
            dropped: 0,
            keys: 0,
            bytes: 0,
        }
    }

    fn maybe_report(&mut self, now: Instant, key: EncoderKey, profile: SendProfile) {
        let elapsed = now.duration_since(self.window_start);
        if elapsed < Duration::from_secs(5) {
            return;
        }
        if crate::call::call_log_enabled() {
            let secs = elapsed.as_secs_f64().max(0.1);
            let out_kbps = (self.bytes as f64 * 8.0 / 1000.0) / secs;
            let enc_fps = f64::from(self.encoded) / secs;
            eprintln!(
                "zalo-gpui call: tx {}x{} @{:.1}fps target={}kbps out={:.0}kbps cap={} enc={} skip={} drop={} key={} rung={}",
                key.width,
                key.height,
                enc_fps,
                key.bitrate_bps / 1000,
                out_kbps,
                self.captured,
                self.encoded,
                self.skipped,
                self.dropped,
                self.keys,
                profile.rung
            );
        }
        *self = Self::new(now);
    }
}

/// Uncompressed local view of the frame about to be encoded.
pub(crate) struct LocalPreview {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// Packed BGRA.
    pub bgra: Arc<Vec<u8>>,
}

/// A v4l2 device being encoded on `zcall-cam`.
pub(crate) struct CameraCapture {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<CamEvent>,
    ctrl_tx: Sender<CamCtrl>,
    wake: Arc<Condvar>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
}

impl CameraCapture {
    /// Starts probing `/dev/video*` with `initial` encode profile, trying
    /// `preferred_device` (e.g. `/dev/video2`) first if present.
    ///
    /// # Errors
    ///
    /// Returns when the capture thread cannot be spawned. A missing camera or
    /// ffmpeg is reported later as [`CamEvent::Failed`].
    pub(crate) fn start(
        initial: SendProfile,
        preferred_device: Option<String>,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        // Three encoded frames is the native queue (`encodeQueueSize < 3`).
        // A deeper queue is what the phone shows as a smeared, late picture.
        let (tx, rx) = mpsc::sync_channel(3);
        let (ctrl_tx, ctrl_rx) = mpsc::channel();
        let wake = Arc::new(Condvar::new());
        let preview = Arc::new(Mutex::new(None));
        let stop_t = Arc::clone(&stop);
        let child_t = Arc::clone(&child);
        let wake_t = Arc::clone(&wake);
        let preview_t = Arc::clone(&preview);
        let handle = std::thread::Builder::new()
            .name("zcall-cam".into())
            .spawn(move || {
                capture_loop(
                    stop_t,
                    child_t,
                    tx,
                    ctrl_rx,
                    wake_t,
                    initial,
                    preferred_device,
                    preview_t,
                )
            })
            .map_err(|err| format!("Không mở được camera: {err}"))?;
        Ok(Self {
            stop,
            child,
            handle: Some(handle),
            rx,
            ctrl_tx,
            wake,
            preview,
        })
    }

    /// Latest local preview, if the capture thread has replaced it.
    pub(crate) fn take_preview(&self) -> Option<LocalPreview> {
        self.preview.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Updates target resolution, fps, and bitrate live without reopening v4l2.
    pub(crate) fn set_profile(&self, profile: SendProfile) {
        let _ = self.ctrl_tx.send(CamCtrl::Profile(profile));
        self.wake.notify_all();
    }

    /// Requests an immediate IDR key frame (on peer/SFU RTCP PLI/FIR).
    pub(crate) fn request_key_frame(&self) {
        let _ = self.ctrl_tx.send(CamCtrl::ForceKey);
        self.wake.notify_all();
    }

    /// The next encoded frame or failure, if the capture thread has one.
    pub(crate) fn poll(&self) -> Option<CamEvent> {
        self.rx.try_recv().ok()
    }

    /// Stops ffmpeg and joins the capture thread.
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.wake.notify_all();
        kill_child(&self.child);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What one device attempt did.
enum Attempt {
    /// Frames were sent, or the caller asked to stop.
    Done,
    /// This device produced nothing.
    Failed,
}

#[derive(Default)]
struct FrameSlot {
    seq: u64,
    frame: Option<Vec<u8>>,
    ended: bool,
}

fn capture_loop(
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    tx: SyncSender<CamEvent>,
    ctrl_rx: Receiver<CamCtrl>,
    wake: Arc<Condvar>,
    mut profile: SendProfile,
    preferred_device: Option<String>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
) {
    let devices = order_video_devices(video_devices(), preferred_device.as_deref());
    if devices.is_empty() {
        let _ = tx.try_send(CamEvent::Failed("Không tìm thấy camera".into()));
        return;
    }
    if !ffmpeg_missing() {
        let attempts = build_attempts(profile.capture_big || profile.width > 640);
        for device in &devices {
            for &spec in &attempts {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                if matches!(
                    try_device(
                        device,
                        spec,
                        &stop,
                        &child,
                        &tx,
                        &ctrl_rx,
                        &wake,
                        &mut profile,
                        &preview,
                    ),
                    Attempt::Done
                ) {
                    return;
                }
            }
        }
    }
    for device in &devices {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if matches!(
            try_v4l(device, &stop, &tx, &ctrl_rx, &mut profile, &preview),
            Attempt::Done
        ) {
            return;
        }
    }
    eprintln!(
        "zalo-gpui call: camera produced no picture from {} device(s)",
        devices.len()
    );
    let _ = tx.try_send(CamEvent::Failed(
        "Không đọc được hình từ camera (đang bị ứng dụng khác giữ?)".into(),
    ));
}

/// Webcam frames through Video4Linux when `ffmpeg` is not installed.
fn try_v4l(
    device: &Path,
    stop: &Arc<AtomicBool>,
    tx: &SyncSender<CamEvent>,
    ctrl_rx: &Receiver<CamCtrl>,
    profile: &mut SendProfile,
    preview: &Arc<Mutex<Option<LocalPreview>>>,
) -> Attempt {
    let Some(mut grab) = V4lGrab::open(device, profile) else {
        return Attempt::Failed;
    };
    let (mut width, mut height, mut yuyv) = (grab.width, grab.height, grab.yuyv);
    let mut tried_big = profile.capture_big || profile.width > 640 || profile.height > 480;
    let mut encoder: Option<Encoder> = None;
    let mut enc_key: Option<EncoderKey> = None;
    let mut enc_initialized = false;
    let mut produced = false;
    let mut force_key = true;
    let mut last_key = Instant::now() - Duration::from_secs(3);
    let mut last_preview = Instant::now() - LOCAL_PREVIEW_INTERVAL;
    let mut next_due = Instant::now();
    let mut last_cap_at: Option<Instant> = None;
    let mut cap_fps_seeded = false;
    let mut cap_fps_est = 30.0_f64;
    let opened = Instant::now();
    let mut tx_stats = TxStats::new(opened);
    while !stop.load(Ordering::SeqCst) {
        while let Ok(msg) = ctrl_rx.try_recv() {
            match msg {
                CamCtrl::Profile(next) => *profile = next,
                CamCtrl::ForceKey => force_key = true,
            }
        }
        let want_big = profile.capture_big || profile.width > 640 || profile.height > 480;
        if want_big && width <= 640 && !tried_big {
            tried_big = true;
            drop(grab);
            if let Some(next_grab) = V4lGrab::open(device, profile) {
                (width, height, yuyv) = (next_grab.width, next_grab.height, next_grab.yuyv);
                grab = next_grab;
                force_key = true;
            } else {
                return if produced { Attempt::Done } else { Attempt::Failed };
            }
        }
        let Some(buf) = grab.latest_frame() else {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if !produced && opened.elapsed() > Duration::from_secs(3) {
                eprintln!(
                    "zalo-gpui call: camera {} produced no frame",
                    device.display()
                );
                return Attempt::Failed;
            }
            continue;
        };
        while let Ok(msg) = ctrl_rx.try_recv() {
            match msg {
                CamCtrl::Profile(next) => *profile = next,
                CamCtrl::ForceKey => force_key = true,
            }
        }
        tx_stats.captured += 1;
        let now = Instant::now();
        if let Some(prev) = last_cap_at {
            let dt = now.duration_since(prev).as_secs_f64();
            if dt > 0.005 && dt < 0.5 {
                let inst_fps = 1.0 / dt;
                cap_fps_est = if cap_fps_seeded {
                    cap_fps_est * 0.85 + inst_fps * 0.15
                } else {
                    cap_fps_seeded = true;
                    inst_fps
                };
            }
        }
        last_cap_at = Some(now);
        // Same gate as `call.js` encodeFrame: a 15 fps rung must not encode
        // every 30 fps capture. Decoding the dropped frames is what made the
        // picture late and smeared.
        let target_fps = f64::from(profile.fps.clamp(8, 30));
        let interval = Duration::from_secs_f64(1.0 / target_fps);
        let (due, due_at) = encode_due(now, next_due, interval);
        if !due && !force_key {
            continue;
        }
        next_due = due_at;
        let Some(i420) = frame_to_i420(&buf, width, height, yuyv) else {
            continue;
        };
        // Keep the call profile's aspect. `target_encode_dims` is for the
        // ffmpeg path, which already cropped. A 640x480 webcam into 640x360
        // must be cropped, not squeezed.
        let (enc_w, enc_h) = v4l_encode_dims(*profile, width, height);
        let actual_fps = target_fps.min(cap_fps_est.clamp(8.0, 30.0)).round() as u32;
        let desired_key = EncoderKey {
            width: enc_w,
            height: enc_h,
            bitrate_bps: profile.bitrate.max(150_000),
            fps_rounded: actual_fps.max(8),
        };
        if needs_encoder_reopen(enc_key, desired_key) {
            match open_encoder(
                enc_w,
                enc_h,
                desired_key.bitrate_bps,
                desired_key.fps_rounded,
            ) {
                Ok(enc) => {
                    encoder = Some(enc);
                    enc_key = Some(desired_key);
                    enc_initialized = false;
                    force_key = true;
                    eprintln!(
                        "zalo-gpui call: encoder {}x{} @{}fps {} kbps (rung {})",
                        enc_w,
                        enc_h,
                        desired_key.fps_rounded,
                        desired_key.bitrate_bps / 1000,
                        profile.rung
                    );
                }
                Err(_) => {
                    return if produced {
                        Attempt::Done
                    } else {
                        Attempt::Failed
                    }
                }
            }
        } else if let (Some(enc), Some(prev)) = (encoder.as_mut(), enc_key) {
            if (prev.bitrate_bps != desired_key.bitrate_bps
                || prev.fps_rounded != desired_key.fps_rounded)
                && enc_initialized
                && update_encoder_rate(enc, desired_key.bitrate_bps, desired_key.fps_rounded)
            {
                enc_key = Some(desired_key);
                if crate::call::call_log_enabled() {
                    eprintln!(
                        "zalo-gpui call: encoder rate -> {} kbps @{}fps (live, rung {})",
                        desired_key.bitrate_bps / 1000,
                        desired_key.fps_rounded,
                        profile.rung
                    );
                }
            }
        }
        let Some(enc) = encoder.as_mut() else {
            continue;
        };
        if force_key
            || last_key.elapsed() >= Duration::from_millis(u64::from(profile.key_ms.max(2_000)))
        {
            enc.force_intra_frame();
            force_key = false;
            last_key = now;
        }
        let (cropped, cw, ch) = crop_i420_center(&i420, width, height, enc_w, enc_h);
        let yuv = if enc_w == cw && enc_h == ch {
            cropped
        } else {
            scale_i420(&cropped, cw, ch, enc_w, enc_h)
        };
        if last_preview.elapsed() >= LOCAL_PREVIEW_INTERVAL {
            if let Ok(mut slot) = preview.lock() {
                *slot = preview_from_i420(&yuv, enc_w, enc_h);
            }
            last_preview = now;
        }
        let picture = YUVBuffer::from_vec(yuv, enc_w, enc_h);
        let ts_ms = now.duration_since(opened).as_millis() as u64;
        let Ok(bitstream) = enc.encode_at(&picture, Timestamp::from_millis(ts_ms)) else {
            continue;
        };
        enc_initialized = true;
        if bitstream.frame_type() == FrameType::Skip {
            tx_stats.skipped += 1;
            tx_stats.maybe_report(now, desired_key, *profile);
            continue;
        }
        let data = bitstream.to_vec();
        if data.is_empty() {
            continue;
        }
        let key = bitstream.frame_type() == FrameType::IDR || annex_b_key(&data);
        if key {
            last_key = now;
            tx_stats.keys += 1;
        }
        let data_len = data.len();
        if !produced {
            produced = true;
            eprintln!(
                "zalo-gpui call: camera {} {}x{} {} (v4l)",
                device.display(),
                width,
                height,
                if yuyv { "yuyv" } else { "mjpg" }
            );
        }
        match tx.try_send(CamEvent::Frame { key, data }) {
            Ok(()) => {
                tx_stats.encoded += 1;
                tx_stats.bytes += data_len;
            }
            Err(TrySendError::Full(_)) => {
                tx_stats.dropped += 1;
                if key {
                    force_key = true;
                }
            }
            Err(TrySendError::Disconnected(_)) => return Attempt::Done,
        }
        tx_stats.maybe_report(now, desired_key, *profile);
    }
    if produced {
        Attempt::Done
    } else {
        Attempt::Failed
    }
}

/// One mapped V4L2 capture.
///
/// `v4l2_format` is 208 bytes. `type` is at 0, then 4 bytes of padding,
/// because the `fmt` union holds pointers and starts at offset 8.
/// Width, height, and fourcc are therefore at 8, 12, and 16 — not 4, 8, 12.
/// `v4l2_buffer` is 88 bytes: index 0, bytesused 8, memory 60, offset 64, length 72.
struct V4lGrab {
    fd: libc::c_int,
    maps: Vec<(*mut libc::c_void, usize)>,
    width: usize,
    height: usize,
    yuyv: bool,
}

const V4L_S_FMT: libc::Ioctl = 3_234_878_981;
const V4L_REQBUFS: libc::Ioctl = 3_222_558_216;
const V4L_QUERYBUF: libc::Ioctl = 3_227_014_665;
const V4L_QBUF: libc::Ioctl = 3_227_014_671;
const V4L_DQBUF: libc::Ioctl = 3_227_014_673;
const V4L_STREAMON: libc::Ioctl = 1_074_026_002;
const V4L_QUERYCAP: libc::Ioctl = 2_154_321_408;

enum FormatTry {
    Ok,
    FormatRejected,
    StreamRejected,
}

impl V4lGrab {
    fn open(path: &Path, profile: &SendProfile) -> Option<Self> {
        let mut logged_open_err = false;
        for attempt in 0..2 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(120));
            }
            let Some(fd) = Self::open_fd(path, !logged_open_err) else {
                logged_open_err = true;
                continue;
            };
            let mut grab = Self {
                fd,
                maps: Vec::new(),
                width: 0,
                height: 0,
                yuyv: true,
            };
            if grab.negotiate(path, profile) {
                return Some(grab);
            }
        }
        eprintln!("zalo-gpui call: camera {} negotiate failed", path.display());
        None
    }

    fn open_fd(path: &Path, log_err: bool) -> Option<libc::c_int> {
        let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            if log_err {
                eprintln!(
                    "zalo-gpui call: camera {} open {}",
                    path.display(),
                    std::io::Error::last_os_error()
                );
            }
            return None;
        }
        Some(fd)
    }

    fn reopen_fd(&mut self, path: &Path) -> bool {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
        if let Some(fd) = Self::open_fd(path, false) {
            self.fd = fd;
            true
        } else {
            false
        }
    }

    fn negotiate(&mut self, path: &Path, profile: &SendProfile) -> bool {
        let sizes = if profile.capture_big || profile.width > 640 || profile.height > 480 {
            [(1280_u32, 720_u32), (640, 480)]
        } else {
            [(640_u32, 480_u32), (1280, 720)]
        };
        let yuyv = (u32::from_le_bytes(*b"YUYV"), true);
        let mjpg = (u32::from_le_bytes(*b"MJPG"), false);
        let mut first = true;
        for &(want_w, want_h) in &sizes {
            // Above 640 px this sensor's YUYV mode is only a few frames a
            // second, so the exposure smears. MJPEG is the mode the original
            // getUserMedia path actually gets at 720p.
            let formats = if want_w > 640 {
                [mjpg, yuyv]
            } else {
                [yuyv, mjpg]
            };
            for (code, is_yuyv) in formats {
                for req_bufs in [4_u32, 2] {
                    if !first && !self.reopen_fd(path) {
                        return false;
                    }
                    first = false;
                    match self.try_format(want_w, want_h, code, is_yuyv, req_bufs) {
                        FormatTry::Ok => return true,
                        FormatTry::FormatRejected => break,
                        FormatTry::StreamRejected => continue,
                    }
                }
            }
        }
        false
    }

    fn try_format(
        &mut self,
        want_w: u32,
        want_h: u32,
        code: u32,
        yuyv: bool,
        req_bufs: u32,
    ) -> FormatTry {
        self.release_bufs();
        let tag_bytes = code.to_le_bytes();
        let tag = std::str::from_utf8(&tag_bytes).unwrap_or("????");
        let mut fmt = vec![0_u8; 208];
        put_u32(&mut fmt, 0, 1);
        put_u32(&mut fmt, 8, want_w);
        put_u32(&mut fmt, 12, want_h);
        put_u32(&mut fmt, 16, code);
        put_u32(&mut fmt, 20, 1);
        if unsafe { libc::ioctl(self.fd, V4L_S_FMT, fmt.as_mut_ptr()) } != 0 {
            eprintln!(
                "zalo-gpui call: camera S_FMT {want_w}x{want_h} {tag} {}",
                std::io::Error::last_os_error()
            );
            return FormatTry::FormatRejected;
        }
        let width = get_u32(&fmt, 8) as usize;
        let height = get_u32(&fmt, 12) as usize;
        let got = get_u32(&fmt, 16);
        if got != code || width < 160 || height < 120 || width % 2 != 0 || height % 2 != 0 {
            let got_bytes = got.to_le_bytes();
            let got_tag = std::str::from_utf8(&got_bytes).unwrap_or("????");
            eprintln!(
                "zalo-gpui call: camera S_FMT mismatch {want_w}x{want_h} {tag} -> {width}x{height} {got_tag}"
            );
            return FormatTry::FormatRejected;
        }
        // 30 fps keeps the shutter short. The encoder still runs at the rung
        // rate and drops the frames in between.
        self.set_frame_rate(30);
        let mut req = vec![0_u8; 20];
        put_u32(&mut req, 0, req_bufs);
        put_u32(&mut req, 4, 1);
        put_u32(&mut req, 8, 1);
        if unsafe { libc::ioctl(self.fd, V4L_REQBUFS, req.as_mut_ptr()) } != 0 {
            eprintln!(
                "zalo-gpui call: camera REQBUFS({req_bufs}) {width}x{height} {tag} {}",
                std::io::Error::last_os_error()
            );
            return FormatTry::StreamRejected;
        }
        let count = get_u32(&req, 0).min(req_bufs);
        if count == 0 {
            eprintln!("zalo-gpui call: camera REQBUFS({req_bufs}) returned 0 buffers for {width}x{height} {tag}");
            self.release_bufs();
            return FormatTry::StreamRejected;
        }
        let mut maps = Vec::new();
        let mut queued = 0_u32;
        for index in 0..count {
            let mut buf = vec![0_u8; 88];
            put_u32(&mut buf, 0, index);
            put_u32(&mut buf, 4, 1);
            put_u32(&mut buf, 60, 1);
            if unsafe { libc::ioctl(self.fd, V4L_QUERYBUF, buf.as_mut_ptr()) } != 0 {
                continue;
            }
            let length = get_u32(&buf, 72) as usize;
            let offset = get_u32(&buf, 64) as libc::off_t;
            if length == 0 {
                continue;
            }
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    self.fd,
                    offset,
                )
            };
            if ptr == libc::MAP_FAILED {
                continue;
            }
            maps.push((ptr, length));
            if unsafe { libc::ioctl(self.fd, V4L_QBUF, buf.as_mut_ptr()) } == 0 {
                queued += 1;
            }
        }
        if queued == 0 {
            eprintln!("zalo-gpui call: camera queued 0/{count} buffers for {width}x{height} {tag}");
            for (ptr, length) in maps {
                unsafe { libc::munmap(ptr, length) };
            }
            self.release_bufs();
            return FormatTry::StreamRejected;
        }
        let mut kind = 1_i32;
        if unsafe { libc::ioctl(self.fd, V4L_STREAMON, &mut kind) } != 0 {
            eprintln!(
                "zalo-gpui call: camera STREAMON(bufs={queued}) {width}x{height} {tag} {}",
                std::io::Error::last_os_error()
            );
            for (ptr, length) in maps {
                unsafe { libc::munmap(ptr, length) };
            }
            self.release_bufs();
            return FormatTry::StreamRejected;
        }
        self.maps = maps;
        self.width = width;
        self.height = height;
        self.yuyv = yuyv;
        FormatTry::Ok
    }

    fn set_frame_rate(&self, fps: u32) {
        const S_PARM: libc::Ioctl = 3_234_616_854;
        let mut parm = vec![0_u8; 204];
        put_u32(&mut parm, 0, 1);
        put_u32(&mut parm, 12, 1);
        put_u32(&mut parm, 16, fps.max(1));
        unsafe { libc::ioctl(self.fd, S_PARM, parm.as_mut_ptr()) };
    }

    fn release_bufs(&self) {
        let mut req = vec![0_u8; 20];
        put_u32(&mut req, 0, 0);
        put_u32(&mut req, 4, 1);
        put_u32(&mut req, 8, 1);
        unsafe { libc::ioctl(self.fd, V4L_REQBUFS, req.as_mut_ptr()) };
    }

    /// The newest queued frame. Older buffers are discarded so a slow encode
    /// does not send a picture that is already hundreds of milliseconds old.
    fn latest_frame(&mut self) -> Option<Vec<u8>> {
        let mut frame = self.read_frame_timeout(40)?;
        while self.frame_pending() {
            match self.read_frame_timeout(0) {
                Some(newer) => frame = newer,
                None => break,
            }
        }
        Some(frame)
    }

    fn frame_pending(&self) -> bool {
        let mut pfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut pfd, 1, 0) > 0 }
    }

    #[cfg(test)]
    fn read_frame(&mut self) -> Option<Vec<u8>> {
        self.read_frame_timeout(1500)
    }

    fn read_frame_timeout(&mut self, wait_ms: i32) -> Option<Vec<u8>> {
        let mut pfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
        if ready <= 0 {
            return None;
        }
        let mut buf = vec![0_u8; 88];
        put_u32(&mut buf, 4, 1);
        put_u32(&mut buf, 60, 1);
        if unsafe { libc::ioctl(self.fd, V4L_DQBUF, buf.as_mut_ptr()) } != 0 {
            return None;
        }
        let index = get_u32(&buf, 0) as usize;
        let bytes = get_u32(&buf, 72) as usize;
        let used = get_u32(&buf, 8) as usize;
        let copy = self.maps.get(index).map(|(ptr, mapped)| {
            let cap = if bytes == 0 {
                *mapped
            } else {
                bytes.min(*mapped)
            };
            let n = if used == 0 { cap } else { used.min(cap) };
            unsafe { std::slice::from_raw_parts(*ptr as *const u8, n).to_vec() }
        });
        unsafe { libc::ioctl(self.fd, V4L_QBUF, buf.as_mut_ptr()) };
        copy
    }
}

impl Drop for V4lGrab {
    fn drop(&mut self) {
        if self.fd >= 0 {
            const STREAMOFF: libc::Ioctl = 1_074_026_003;
            let mut kind = 1_i32;
            unsafe { libc::ioctl(self.fd, STREAMOFF, &mut kind) };
        }
        for (ptr, length) in self.maps.drain(..) {
            unsafe { libc::munmap(ptr, length) };
        }
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
        }
    }
}

fn put_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_ne_bytes());
}

fn get_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(buf[at..at + 4].try_into().unwrap_or([0; 4]))
}

fn frame_to_i420(buf: &[u8], width: usize, height: usize, yuyv: bool) -> Option<Vec<u8>> {
    if yuyv {
        let need = width.checked_mul(height)?.checked_mul(2)?;
        if buf.len() < need || width % 2 != 0 || height % 2 != 0 {
            return None;
        }
        return Some(yuyv_to_i420(&buf[..need], width, height));
    }
    let rgb = image::load_from_memory(buf).ok()?.into_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    if w % 2 != 0 || h % 2 != 0 {
        return None;
    }
    Some(rgb_to_i420(rgb.as_raw(), w, h))
}

fn yuyv_to_i420(src: &[u8], width: usize, height: usize) -> Vec<u8> {
    let y_len = width * height;
    let c_len = y_len / 4;
    let mut out = vec![0_u8; y_len + c_len * 2];
    let (y, rest) = out.split_at_mut(y_len);
    let (u, v) = rest.split_at_mut(c_len);
    let cw = width / 2;
    let row_bytes = width * 2;
    for row in (0..height).step_by(2) {
        let s0 = row * row_bytes;
        let s1 = s0 + row_bytes;
        let y0 = row * width;
        let y1 = y0 + width;
        let c_row = (row / 2) * cw;
        for col in (0..width).step_by(2) {
            let off = col * 2;
            y[y0 + col] = src[s0 + off];
            y[y0 + col + 1] = src[s0 + off + 2];
            y[y1 + col] = src[s1 + off];
            y[y1 + col + 1] = src[s1 + off + 2];
            // Average vertical chroma pairs (4:2:2 -> 4:2:0) instead of dropping
            // odd rows, preventing color fringes on horizontal edges.
            let c = c_row + col / 2;
            u[c] = ((u16::from(src[s0 + off + 1]) + u16::from(src[s1 + off + 1]) + 1) >> 1) as u8;
            v[c] = ((u16::from(src[s0 + off + 3]) + u16::from(src[s1 + off + 3]) + 1) >> 1) as u8;
        }
    }
    out
}

fn rgb_to_i420(rgb: &[u8], width: usize, height: usize) -> Vec<u8> {
    let y_len = width * height;
    let c_len = (width / 2) * (height / 2);
    let mut out = vec![0_u8; y_len + c_len * 2];
    let (y, rest) = out.split_at_mut(y_len);
    let (u, v) = rest.split_at_mut(c_len);
    for row in 0..height {
        let row_off = row * width;
        for col in 0..width {
            let p = (row_off + col) * 3;
            let r = u32::from(rgb[p]);
            let g = u32::from(rgb[p + 1]);
            let b = u32::from(rgb[p + 2]);
            y[row_off + col] =
                ((66 * r + 129 * g + 25 * b + 128) >> 8).saturating_add(16) as u8;
        }
    }
    let cw = width / 2;
    let ch = height / 2;
    for cy in 0..ch {
        let r0 = (cy * 2) * width;
        let r1 = r0 + width;
        for cx in 0..cw {
            let c0 = cx * 2;
            let c1 = c0 + 1;
            let p00 = (r0 + c0) * 3;
            let p01 = (r0 + c1) * 3;
            let p10 = (r1 + c0) * 3;
            let p11 = (r1 + c1) * 3;
            let r = (i32::from(rgb[p00])
                + i32::from(rgb[p01])
                + i32::from(rgb[p10])
                + i32::from(rgb[p11])
                + 2)
                >> 2;
            let g = (i32::from(rgb[p00 + 1])
                + i32::from(rgb[p01 + 1])
                + i32::from(rgb[p10 + 1])
                + i32::from(rgb[p11 + 1])
                + 2)
                >> 2;
            let b = (i32::from(rgb[p00 + 2])
                + i32::from(rgb[p01 + 2])
                + i32::from(rgb[p10 + 2])
                + i32::from(rgb[p11 + 2])
                + 2)
                >> 2;
            let idx = cy * cw + cx;
            u[idx] = ((-38 * r - 74 * g + 112 * b + 128) >> 8)
                .saturating_add(128)
                .clamp(0, 255) as u8;
            v[idx] = ((112 * r - 94 * g - 18 * b + 128) >> 8)
                .saturating_add(128)
                .clamp(0, 255) as u8;
        }
    }
    out
}

fn build_attempts(want_big: bool) -> Vec<AttemptSpec> {
    let mut specs = Vec::new();
    if want_big {
        specs.push(AttemptSpec {
            cap_w: 1280,
            cap_h: 720,
            input_size: Some("1280x720"),
            input_format: Some("mjpeg"),
            fps: Some(30),
            timeout: Duration::from_secs(2),
        });
    }
    specs.push(AttemptSpec {
        cap_w: 640,
        cap_h: 360,
        input_size: Some("640x480"),
        input_format: Some("mjpeg"),
        fps: Some(30),
        timeout: Duration::from_secs(2),
    });
    specs.push(AttemptSpec {
        cap_w: 640,
        cap_h: 360,
        input_size: None,
        input_format: None,
        fps: Some(15),
        timeout: Duration::from_secs(4),
    });
    specs.push(AttemptSpec {
        cap_w: 640,
        cap_h: 360,
        input_size: None,
        input_format: None,
        fps: None,
        timeout: Duration::from_secs(4),
    });
    specs
}

#[allow(clippy::too_many_arguments)]
fn try_device(
    device: &Path,
    spec: AttemptSpec,
    stop: &Arc<AtomicBool>,
    child_slot: &Arc<Mutex<Option<Child>>>,
    tx: &SyncSender<CamEvent>,
    ctrl_rx: &Receiver<CamCtrl>,
    wake: &Arc<Condvar>,
    profile: &mut SendProfile,
    preview: &Arc<Mutex<Option<LocalPreview>>>,
) -> Attempt {
    let Ok(mut process) = ffmpeg_cmd(device, spec).spawn() else {
        return Attempt::Failed;
    };
    let Some(mut stdout) = process.stdout.take() else {
        let _ = process.kill();
        let _ = process.wait();
        return Attempt::Failed;
    };
    *child_slot.lock().unwrap_or_else(|err| err.into_inner()) = Some(process);

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_t = Arc::clone(&cancel);
    let stop_t = Arc::clone(stop);
    let slot_t = Arc::clone(child_slot);
    std::thread::spawn(move || {
        let deadline = Instant::now() + spec.timeout;
        while Instant::now() < deadline {
            if cancel_t.load(Ordering::SeqCst) || stop_t.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !cancel_t.load(Ordering::SeqCst) {
            kill_child(&slot_t);
        }
    });

    let frame_bytes = spec.cap_w * spec.cap_h * 3 / 2;
    let frame_slot = Arc::new(Mutex::new(FrameSlot::default()));
    let frame_slot_r = Arc::clone(&frame_slot);
    let wake_r = Arc::clone(wake);
    let stop_r = Arc::clone(stop);
    let reader_handle = std::thread::Builder::new()
        .name("zcall-cam-read".into())
        .spawn(move || {
            let mut buf = vec![0_u8; frame_bytes];
            while !stop_r.load(Ordering::SeqCst) {
                if stdout.read_exact(&mut buf).is_err() {
                    break;
                }
                let mut guard = frame_slot_r.lock().unwrap_or_else(|e| e.into_inner());
                guard.seq = guard.seq.wrapping_add(1);
                guard.frame = Some(buf.clone());
                wake_r.notify_all();
            }
            let mut guard = frame_slot_r.lock().unwrap_or_else(|e| e.into_inner());
            guard.ended = true;
            wake_r.notify_all();
        })
        .ok();

    let mut produced = false;
    let mut seen_seq = 0_u64;
    let mut held_frame: Option<Vec<u8>> = None;
    let mut held_at = Instant::now();
    let mut repeat_at = Instant::now();
    let mut last_cap_at: Option<Instant> = None;
    let mut cap_fps_est = 30.0_f64;
    let mut next_due = Instant::now();
    let mut last_key_at = Instant::now();
    let mut last_preview = Instant::now() - LOCAL_PREVIEW_INTERVAL;
    let mut force_key = true;
    let mut enc_key: Option<EncoderKey> = None;
    let mut enc_initialized = false;
    let mut encoder: Option<Encoder> = None;
    let started_at = Instant::now();
    let mut cap_fps_seeded = false;
    let mut tx_stats = TxStats::new(started_at);

    let finish = |produced: bool| {
        cancel.store(true, Ordering::SeqCst);
        kill_child(child_slot);
        if let Some(handle) = reader_handle {
            let _ = handle.join();
        }
        if produced {
            Attempt::Done
        } else {
            Attempt::Failed
        }
    };

    loop {
        if stop.load(Ordering::SeqCst) {
            return finish(true);
        }
        while let Ok(msg) = ctrl_rx.try_recv() {
            match msg {
                CamCtrl::Profile(next_profile) => *profile = next_profile,
                CamCtrl::ForceKey => force_key = true,
            }
        }

        let (fresh_frame, ended) = {
            let mut guard = frame_slot.lock().unwrap_or_else(|e| e.into_inner());
            while !stop.load(Ordering::SeqCst) && guard.seq == seen_seq && !guard.ended {
                let wait_for = if produced {
                    STALL_REPEAT_STEP
                } else {
                    Duration::from_millis(100)
                };
                let (next_guard, _) = wake
                    .wait_timeout(guard, wait_for)
                    .unwrap_or_else(|e| e.into_inner());
                guard = next_guard;
                while let Ok(msg) = ctrl_rx.try_recv() {
                    match msg {
                        CamCtrl::Profile(next_profile) => *profile = next_profile,
                        CamCtrl::ForceKey => force_key = true,
                    }
                }
                if force_key && produced {
                    break;
                }
            }
            if guard.ended && guard.seq == seen_seq {
                (None, true)
            } else if guard.seq != seen_seq {
                seen_seq = guard.seq;
                (guard.frame.take(), false)
            } else {
                (None, false)
            }
        };

        if ended {
            return finish(produced);
        }

        let mut is_repeat = false;
        let source_frame = if let Some(frame) = fresh_frame {
            if !produced {
                cancel.store(true, Ordering::SeqCst);
                produced = true;
                eprintln!(
                    "zalo-gpui call: camera {} ({}x{})",
                    device.display(),
                    spec.cap_w,
                    spec.cap_h
                );
            }
            let now = Instant::now();
            if let Some(prev) = last_cap_at {
                let dt = now.duration_since(prev).as_secs_f64();
                if dt > 0.005 && dt < 0.5 {
                    let inst_fps = 1.0 / dt;
                    cap_fps_est = if cap_fps_seeded {
                        cap_fps_est * 0.85 + inst_fps * 0.15
                    } else {
                        cap_fps_seeded = true;
                        inst_fps
                    };
                }
            }
            tx_stats.captured += 1;
            last_cap_at = Some(now);
            held_frame = Some(frame.clone());
            held_at = now;
            frame
        } else if force_key && held_frame.is_some() {
            is_repeat = true;
            held_frame.clone().unwrap()
        } else if let Some(held) = held_frame.as_ref() {
            if held_at.elapsed() >= STALL_REPEAT_AFTER && repeat_at.elapsed() >= STALL_REPEAT_STEP {
                repeat_at = Instant::now();
                is_repeat = true;
                held.clone()
            } else {
                continue;
            }
        } else {
            continue;
        };

        let target_fps = f64::from(profile.fps.clamp(5, 30));
        let interval = Duration::from_secs_f64(1.0 / target_fps);
        let now = Instant::now();
        if !is_repeat {
            if now + interval / 2 < next_due {
                continue;
            }
            next_due = (next_due + interval).max(now - interval / 2);
        }

        let (enc_w, enc_h) = target_encode_dims(*profile, spec.cap_w, spec.cap_h);
        let actual_fps = target_fps.min(cap_fps_est.clamp(8.0, 30.0)).round() as u32;
        let desired_key = EncoderKey {
            width: enc_w,
            height: enc_h,
            bitrate_bps: profile.bitrate.max(150_000),
            fps_rounded: actual_fps.max(8),
        };
        if needs_encoder_reopen(enc_key, desired_key) {
            match open_encoder(
                enc_w,
                enc_h,
                desired_key.bitrate_bps,
                desired_key.fps_rounded,
            ) {
                Ok(new_enc) => {
                    eprintln!(
                        "zalo-gpui call: encoder {}x{} @{}fps {} kbps (rung {})",
                        enc_w,
                        enc_h,
                        desired_key.fps_rounded,
                        desired_key.bitrate_bps / 1000,
                        profile.rung
                    );
                    encoder = Some(new_enc);
                    enc_key = Some(desired_key);
                    enc_initialized = false;
                    force_key = true;
                }
                Err(_) => {
                    return finish(produced);
                }
            }
        } else if let (Some(enc), Some(prev)) = (encoder.as_mut(), enc_key) {
            if (prev.bitrate_bps != desired_key.bitrate_bps
                || prev.fps_rounded != desired_key.fps_rounded)
                && enc_initialized
                && update_encoder_rate(enc, desired_key.bitrate_bps, desired_key.fps_rounded)
            {
                enc_key = Some(desired_key);
                if crate::call::call_log_enabled() {
                    eprintln!(
                        "zalo-gpui call: encoder rate -> {} kbps @{}fps (live, rung {})",
                        desired_key.bitrate_bps / 1000,
                        desired_key.fps_rounded,
                        profile.rung
                    );
                }
            }
        }
        let Some(enc) = encoder.as_mut() else {
            return finish(produced);
        };

        let key_interval = Duration::from_millis(u64::from(profile.key_ms.max(2_000)));
        if force_key || last_key_at.elapsed() >= key_interval {
            enc.force_intra_frame();
            force_key = false;
            last_key_at = now;
        }

        let encode_yuv = if enc_w == spec.cap_w && enc_h == spec.cap_h {
            source_frame
        } else {
            scale_i420(&source_frame, spec.cap_w, spec.cap_h, enc_w, enc_h)
        };
        if last_preview.elapsed() >= LOCAL_PREVIEW_INTERVAL {
            if let Ok(mut slot) = preview.lock() {
                *slot = preview_from_i420(&encode_yuv, enc_w, enc_h);
            }
            last_preview = now;
        }
        let picture = YUVBuffer::from_vec(encode_yuv, enc_w, enc_h);
        let ts_ms = now.duration_since(started_at).as_millis() as u64;
        let Ok(bitstream) = enc.encode_at(&picture, Timestamp::from_millis(ts_ms)) else {
            continue;
        };
        enc_initialized = true;
        if bitstream.frame_type() == FrameType::Skip {
            tx_stats.skipped += 1;
            tx_stats.maybe_report(now, desired_key, *profile);
            continue;
        }
        let data = bitstream.to_vec();
        if data.is_empty() {
            continue;
        }
        let key = bitstream.frame_type() == FrameType::IDR || annex_b_key(&data);
        if key {
            last_key_at = now;
            tx_stats.keys += 1;
        }
        let data_len = data.len();
        match tx.try_send(CamEvent::Frame { key, data }) {
            Ok(()) => {
                tx_stats.encoded += 1;
                tx_stats.bytes += data_len;
            }
            Err(TrySendError::Full(_)) => {
                tx_stats.dropped += 1;
                if key {
                    force_key = true;
                }
                continue;
            }
            Err(TrySendError::Disconnected(_)) => return finish(produced),
        }
        tx_stats.maybe_report(now, desired_key, *profile);
    }
}

fn target_encode_dims(profile: SendProfile, cap_w: usize, cap_h: usize) -> (usize, usize) {
    let w = (profile.width as usize).min(cap_w).max(16) & !1;
    let h = (profile.height as usize).min(cap_h).max(16) & !1;
    (w, h)
}

/// Encode size for a raw webcam frame. Prefer the call profile when it fits
/// inside the captured picture, so 640×360 stays 16:9 instead of becoming 640×480.
fn v4l_encode_dims(profile: SendProfile, cap_w: usize, cap_h: usize) -> (usize, usize) {
    let pw = (profile.width as usize).max(16) & !1;
    let ph = (profile.height as usize).max(16) & !1;
    if cap_w < 16 || cap_h < 16 {
        return (pw, ph);
    }
    if pw <= cap_w && ph <= cap_h {
        return (pw, ph);
    }
    let w = pw as u64;
    let h = ph as u64;
    let (ew, eh) = if (cap_w as u64) * h <= (cap_h as u64) * w {
        (cap_w, ((cap_w as u64) * h / w) as usize)
    } else {
        (((cap_h as u64) * w / h) as usize, cap_h)
    };
    ((ew.max(16) & !1).min(cap_w), (eh.max(16) & !1).min(cap_h))
}

/// `call.js` encodeFrame gate. `true` means this capture frame is encoded.
fn encode_due(now: Instant, next_due: Instant, interval: Duration) -> (bool, Instant) {
    if interval.is_zero() {
        return (true, now);
    }
    if now + interval / 2 < next_due {
        return (false, next_due);
    }
    (true, (next_due + interval).max(now - interval / 2))
}

/// Local preview, long edge at most 640 so the call window is not uploaded a 720p texture.
fn preview_from_i420(i420: &[u8], w: usize, h: usize) -> Option<LocalPreview> {
    let (pw, ph, owned) = if w.max(h) > 640 && w > 0 && h > 0 {
        let scale = 640.0 / w.max(h) as f32;
        let pw = ((w as f32 * scale).round() as usize).max(2) & !1;
        let ph = ((h as f32 * scale).round() as usize).max(2) & !1;
        (pw, ph, scale_i420(i420, w, h, pw, ph))
    } else {
        (w, h, i420.to_vec())
    };
    let bgra = i420_to_bgra(&owned, pw, ph)?;
    Some(LocalPreview {
        w: pw as u32,
        h: ph as u32,
        bgra: Arc::new(bgra),
    })
}

fn i420_to_bgra(i420: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
    let y_len = w.checked_mul(h)?;
    let c_len = y_len / 4;
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
    Some(bgra)
}

/// Center-crop packed I420 to `aspect_w`:`aspect_h`. Offsets stay even so the chroma planes align.
fn crop_i420_center(
    src: &[u8],
    sw: usize,
    sh: usize,
    aspect_w: usize,
    aspect_h: usize,
) -> (Vec<u8>, usize, usize) {
    let y_len = sw.saturating_mul(sh);
    let c_len = y_len / 4;
    if aspect_w == 0
        || aspect_h == 0
        || sw < 2
        || sh < 2
        || sw % 2 != 0
        || sh % 2 != 0
        || src.len() < y_len + c_len * 2
    {
        return (src.to_vec(), sw, sh);
    }
    let (mut cw, mut ch) = if (sw as u64) * (aspect_h as u64) > (sh as u64) * (aspect_w as u64) {
        let cw = ((sh as u64) * (aspect_w as u64) / (aspect_h as u64)) as usize;
        (cw & !1, sh & !1)
    } else {
        let ch = ((sw as u64) * (aspect_h as u64) / (aspect_w as u64)) as usize;
        (sw & !1, ch & !1)
    };
    cw = cw.clamp(2, sw) & !1;
    ch = ch.clamp(2, sh) & !1;
    if cw == sw && ch == sh {
        return (src.to_vec(), sw, sh);
    }
    let x = ((sw - cw) / 2) & !1;
    let y = ((sh - ch) / 2) & !1;
    let dy_len = cw * ch;
    let du_w = cw / 2;
    let du_h = ch / 2;
    let du_len = du_w * du_h;
    let mut dst = vec![0_u8; dy_len + du_len * 2];
    copy_plane(&src[..y_len], sw, x, y, &mut dst[..dy_len], cw, ch);
    let su_w = sw / 2;
    let su_h = sh / 2;
    let su = &src[y_len..y_len + su_w * su_h];
    let sv = &src[y_len + c_len..y_len + c_len + su_w * su_h];
    copy_plane(
        su,
        su_w,
        x / 2,
        y / 2,
        &mut dst[dy_len..dy_len + du_len],
        du_w,
        du_h,
    );
    copy_plane(
        sv,
        su_w,
        x / 2,
        y / 2,
        &mut dst[dy_len + du_len..],
        du_w,
        du_h,
    );
    (dst, cw, ch)
}

fn copy_plane(
    src: &[u8],
    stride: usize,
    sx: usize,
    sy: usize,
    dst: &mut [u8],
    dw: usize,
    dh: usize,
) {
    for row in 0..dh {
        let s = (sy + row) * stride + sx;
        let d = row * dw;
        dst[d..d + dw].copy_from_slice(&src[s..s + dw]);
    }
}

/// Fast integer bilinear downscale for packed I420 (`yuv420p`).
fn scale_i420(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    if sw == dw && sh == dh {
        return src.to_vec();
    }
    let sy_len = sw * sh;
    let su_w = sw / 2;
    let su_h = sh / 2;
    let su_len = su_w * su_h;

    let dy_len = dw * dh;
    let du_w = dw / 2;
    let du_h = dh / 2;
    let du_len = du_w * du_h;
    let mut dst = vec![0_u8; dy_len + 2 * du_len];

    scale_plane_bilinear(&src[..sy_len], sw, sh, &mut dst[..dy_len], dw, dh);
    let su_off = sy_len;
    let sv_off = sy_len + su_len;
    let du_off = dy_len;
    let dv_off = dy_len + du_len;
    scale_plane_bilinear(
        &src[su_off..su_off + su_len],
        su_w,
        su_h,
        &mut dst[du_off..du_off + du_len],
        du_w,
        du_h,
    );
    scale_plane_bilinear(
        &src[sv_off..sv_off + su_len],
        su_w,
        su_h,
        &mut dst[dv_off..dv_off + du_len],
        du_w,
        du_h,
    );
    dst
}

fn scale_plane_bilinear(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let max_x = (sw - 1) << 16;
    let max_y = (sh - 1) << 16;
    let step_x = if dw > 1 {
        ((sw - 1) << 16) / (dw - 1)
    } else {
        0
    };
    let step_y = if dh > 1 {
        ((sh - 1) << 16) / (dh - 1)
    } else {
        0
    };
    let mut xs = Vec::with_capacity(dw);
    for col in 0..dw {
        let fx = (col * step_x).min(max_x);
        let x0 = fx >> 16;
        let x1 = (x0 + 1).min(sw - 1);
        let wx = ((fx & 0xffff) >> 8) as u32;
        xs.push((x0, x1, wx));
    }

    for row in 0..dh {
        let fy = (row * step_y).min(max_y);
        let y0 = fy >> 16;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = ((fy & 0xffff) >> 8) as u32;
        let iwy = 256 - wy;
        let r0 = y0 * sw;
        let r1 = y1 * sw;
        let d_row = row * dw;
        for (col, &(x0, x1, wx)) in xs.iter().enumerate() {
            let iwx = 256 - wx;
            let p00 = u32::from(src[r0 + x0]);
            let p01 = u32::from(src[r0 + x1]);
            let p10 = u32::from(src[r1 + x0]);
            let p11 = u32::from(src[r1 + x1]);
            let top = p00 * iwx + p01 * wx;
            let bot = p10 * iwx + p11 * wx;
            dst[d_row + col] = ((top * iwy + bot * wy + 32768) >> 16) as u8;
        }
    }
}

fn annex_b_key(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let nal = data[i + 3] & 0x1f;
            if nal == 5 || nal == 7 || nal == 8 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

fn open_encoder(
    width: usize,
    height: usize,
    bitrate_bps: u32,
    fps: u32,
) -> Result<Encoder, openh264::Error> {
    let api = OpenH264API::from_source();
    let level = if width > 640 || height > 480 {
        Level::Level_4_0
    } else {
        Level::Level_3_0
    };
    let complexity = if width * height <= 640 * 360 {
        Complexity::High
    } else {
        Complexity::Medium
    };
    let qp = if bitrate_bps <= 350_000 {
        QpRange::new(16, 42)
    } else if bitrate_bps <= 750_000 {
        QpRange::new(14, 38)
    } else {
        QpRange::new(12, 36)
    };
    let config = EncoderConfig::new()
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(0))
        .profile(Profile::Baseline)
        .level(level)
        .complexity(complexity)
        .qp(qp)
        .sps_pps_strategy(SpsPpsStrategy::ConstantId)
        .max_slice_len(1060)
        .adaptive_quantization(true)
        .background_detection(true)
        .scene_change_detect(true)
        // Keeping skip off + Bitrate RC + MTU-sized slices prevents VBV drops,
        // macroblock blur, and full-frame corruption on single UDP loss.
        .skip_frames(false)
        .num_threads(if width * height > 640 * 360 { 2 } else { 1 });
    Encoder::with_api_config(api, config)
}

fn ffmpeg_missing() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
}

fn video_devices() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(dir) = std::fs::read_dir("/dev") else {
        return found;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(index) = name.strip_prefix("video") else {
            continue;
        };
        if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let path = entry.path();
        if !device_is_capture(&path) {
            continue;
        }
        found.push(path);
    }
    found.sort_by_key(|path| {
        path.file_name()
            .and_then(|name| {
                name.to_string_lossy()
                    .strip_prefix("video")
                    .map(str::to_owned)
            })
            .and_then(|index| index.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    found
}

/// True when the node advertises `V4L2_CAP_VIDEO_CAPTURE`.
///
/// The SPCA2650 exposes `/dev/video1` as metadata only. Opening it does not
/// produce pictures, so it must not appear as a second camera.
fn device_is_capture(path: &Path) -> bool {
    let Ok(c_path) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
        return false;
    };
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        return false;
    }
    let mut cap = vec![0_u8; 104];
    let ok = unsafe { libc::ioctl(fd, V4L_QUERYCAP, cap.as_mut_ptr()) } == 0;
    unsafe { libc::close(fd) };
    if !ok {
        return false;
    }
    let caps = get_u32(&cap, 84);
    let dev = get_u32(&cap, 88);
    let bits = if caps & 0x8000_0000 != 0 { dev } else { caps };
    bits & 0x1 != 0
}

/// Friendly option string for a V4L2 device node (`"/dev/video0 — Camera"`).
#[must_use]
pub(crate) fn format_camera_option(path: &Path, sys_name: Option<&str>) -> String {
    let dev = path.display();
    match sys_name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => format!("{dev} — {name}"),
        None => dev.to_string(),
    }
}

/// Enumerates V4L2 camera options for the Settings dropdown (skipped in unit tests).
#[must_use]
pub(crate) fn camera_option_labels() -> Vec<String> {
    if cfg!(test) {
        return Vec::new();
    }
    video_devices()
        .into_iter()
        .map(|path| {
            let sys = path.file_name().and_then(|n| n.to_str()).and_then(|node| {
                std::fs::read_to_string(format!("/sys/class/video4linux/{node}/name")).ok()
            });
            format_camera_option(&path, sys.as_deref())
        })
        .collect()
}

/// Extracts `/dev/...` from a saved `"cameraDevice"` preference string.
#[must_use]
pub(crate) fn parse_camera_path(choice: &str) -> Option<String> {
    let trimmed = choice.trim();
    if trimmed.is_empty() || trimmed == "Mặc định" {
        return None;
    }
    let path = trimmed.split(" — ").next().unwrap_or(trimmed).trim();
    path.starts_with("/dev/").then(|| path.to_owned())
}

/// Places `preferred` first in `devices` when it matches a detected node, keeping fallbacks.
#[must_use]
pub(crate) fn order_video_devices(
    mut devices: Vec<PathBuf>,
    preferred: Option<&str>,
) -> Vec<PathBuf> {
    let Some(target) = preferred.and_then(parse_camera_path) else {
        return devices;
    };
    if let Some(pos) = devices.iter().position(|p| p.to_string_lossy() == target) {
        let picked = devices.remove(pos);
        devices.insert(0, picked);
    }
    devices
}

fn ffmpeg_cmd(device: &Path, spec: AttemptSpec) -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-fflags")
        .arg("nobuffer")
        .arg("-flags")
        .arg("low_delay")
        .arg("-probesize")
        .arg("32")
        .arg("-analyzeduration")
        .arg("0")
        .arg("-f")
        .arg("v4l2");
    if let Some(fmt) = spec.input_format {
        cmd.arg("-input_format").arg(fmt);
    }
    if let Some(size) = spec.input_size {
        cmd.arg("-video_size").arg(size);
    }
    if let Some(fps) = spec.fps {
        cmd.arg("-framerate").arg(fps.to_string());
    }
    let vf = format!(
        "scale={}:{}:force_original_aspect_ratio=increase:flags=bilinear,crop={}:{},format=yuv420p",
        spec.cap_w, spec.cap_h, spec.cap_w, spec.cap_h
    );
    cmd.arg("-i")
        .arg(device)
        .arg("-vf")
        .arg(vf)
        .arg("-an")
        .arg("-f")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd
}

fn kill_child(slot: &Mutex<Option<Child>>) {
    let child = slot.lock().unwrap_or_else(|err| err.into_inner()).take();
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_360p_and_720p_annex_b() {
        for (w, h, bps, fps) in [(640, 360, 500_000, 15), (1280, 720, 1_100_000, 24)] {
            let mut encoder = open_encoder(w, h, bps, fps).expect("openh264 encoder");
            encoder.force_intra_frame();
            let picture = YUVBuffer::from_vec(vec![0_u8; w * h * 3 / 2], w, h);
            let bitstream = encoder.encode(&picture).expect("encode");
            let data = bitstream.to_vec();
            assert!(
                data.starts_with(&[0, 0, 0, 1]),
                "{w}x{h} start {:02x?}",
                &data[..data.len().min(8)]
            );
            assert!(annex_b_key(&data));
        }
    }

    #[test]
    fn scale_i420_produces_expected_luma_and_chroma_size() {
        let mut src = vec![0_u8; 1280 * 720 * 3 / 2];
        src[..1280 * 720].fill(180);
        let dst = scale_i420(&src, 1280, 720, 640, 360);
        assert_eq!(dst.len(), 640 * 360 * 3 / 2);
        assert!(dst[..640 * 360].iter().all(|&y| y == 180));
    }

    #[test]
    fn camera_option_parsing_and_ordering() {
        let label = format_camera_option(Path::new("/dev/video2"), Some("Integrated Webcam\n"));
        assert_eq!(label, "/dev/video2 — Integrated Webcam");
        assert_eq!(parse_camera_path(&label).as_deref(), Some("/dev/video2"));
        assert_eq!(parse_camera_path("Mặc định"), None);
        assert_eq!(parse_camera_path(""), None);

        let devices = vec![
            PathBuf::from("/dev/video0"),
            PathBuf::from("/dev/video2"),
            PathBuf::from("/dev/video4"),
        ];
        let ordered = order_video_devices(devices, Some(&label));
        assert_eq!(
            ordered,
            vec![
                PathBuf::from("/dev/video2"),
                PathBuf::from("/dev/video0"),
                PathBuf::from("/dev/video4"),
            ]
        );
    }

    #[test]
    fn encode_due_skips_the_frames_between_rung_ticks() {
        let t0 = Instant::now();
        let iv = Duration::from_millis(66);
        let (due, next) = encode_due(t0, t0, iv);
        assert!(due);
        assert_eq!(next, t0 + iv);
        let early = t0 + Duration::from_millis(20);
        let (due, kept) = encode_due(early, next, iv);
        assert!(!due);
        assert_eq!(kept, next);
        let on_time = t0 + Duration::from_millis(40);
        let (due, _) = encode_due(on_time, next, iv);
        assert!(due);
    }

    #[test]
    fn v4l_crop_keeps_the_center_band_for_16_by_9() {
        let profile = SendProfile {
            width: 640,
            height: 360,
            fps: 15,
            bitrate: 500_000,
            key_ms: 5_000,
            capture_big: false,
            rung: 0,
        };
        assert_eq!(v4l_encode_dims(profile, 640, 480), (640, 360));
        let mut src = vec![11_u8; 8 * 8 * 3 / 2];
        for row in 2..6 {
            src[row * 8..(row + 1) * 8].fill(77);
        }
        let (dst, w, h) = crop_i420_center(&src, 8, 8, 2, 1);
        assert_eq!((w, h), (8, 4));
        assert!(dst[..32].iter().all(|&y| y == 77));
        assert_eq!(dst.len(), 8 * 4 * 3 / 2);
        let wide = vec![40_u8; 640 * 480 * 3 / 2];
        let (dst, w, h) = crop_i420_center(&wide, 640, 480, 640, 360);
        assert_eq!((w, h), (640, 360));
        assert_eq!(dst.len(), 640 * 360 * 3 / 2);
    }

    #[test]
    fn v4l_webcam_encodes_one_frame_when_present() {
        let path = Path::new("/dev/video0");
        if !path.exists() || !device_is_capture(path) {
            return;
        }
        let profile = SendProfile {
            width: 640,
            height: 360,
            fps: 15,
            bitrate: 500_000,
            key_ms: 5_000,
            capture_big: false,
            rung: 0,
        };
        let mut grab = V4lGrab::open(path, &profile).expect("open webcam");
        let buf = grab.read_frame().expect("webcam frame");
        let i420 = frame_to_i420(&buf, grab.width, grab.height, grab.yuyv).expect("i420");
        let (enc_w, enc_h) = v4l_encode_dims(profile, grab.width, grab.height);
        let (cropped, cw, ch) = crop_i420_center(&i420, grab.width, grab.height, enc_w, enc_h);
        let yuv = if enc_w == cw && enc_h == ch {
            cropped
        } else {
            scale_i420(&cropped, cw, ch, enc_w, enc_h)
        };
        let mut encoder =
            open_encoder(enc_w, enc_h, profile.bitrate, profile.fps).expect("encoder");
        encoder.force_intra_frame();
        let picture = YUVBuffer::from_vec(yuv, enc_w, enc_h);
        let data = encoder.encode(&picture).expect("encode").to_vec();
        assert!(data.starts_with(&[0, 0, 0, 1]));
        assert!(annex_b_key(&data));
    }

    #[test]
    fn needs_encoder_reopen_only_reopens_on_size_changes() {
        let base = EncoderKey {
            width: 640,
            height: 360,
            bitrate_bps: 300_000,
            fps_rounded: 15,
        };
        assert!(needs_encoder_reopen(None, base));
        assert!(!needs_encoder_reopen(Some(base), base));
        // Bitrate and FPS changes update live via SetOption without reopening.
        assert!(!needs_encoder_reopen(
            Some(base),
            EncoderKey {
                bitrate_bps: 500_000,
                ..base
            }
        ));
        assert!(!needs_encoder_reopen(
            Some(base),
            EncoderKey {
                fps_rounded: 24,
                ..base
            }
        ));
        assert!(needs_encoder_reopen(
            Some(base),
            EncoderKey {
                width: 1280,
                height: 720,
                ..base
            }
        ));
    }

    #[test]
    fn live_bitrate_and_fps_update_works_without_reopen_or_idr() {
        let (w, h) = (320, 180);
        let mut enc = open_encoder(w, h, 350_000, 15).expect("open encoder");
        let frame = YUVBuffer::from_vec(vec![128_u8; w * h * 3 / 2], w, h);
        let first = enc
            .encode_at(&frame, Timestamp::from_millis(0))
            .expect("first frame");
        assert_eq!(first.frame_type(), FrameType::IDR);
        // Update bitrate and FPS live without reopening encoder.
        assert!(update_encoder_rate(&mut enc, 750_000, 24));
        let second = enc
            .encode_at(&frame, Timestamp::from_millis(42))
            .expect("second frame");
        assert_eq!(
            second.frame_type(),
            FrameType::P,
            "live rate update must not force an unexpected IDR spike"
        );
    }

    #[test]
    fn yuyv_to_i420_averages_vertical_chroma() {
        // 2x2 YUYV: row0 has U=100, V=200; row1 has U=140, V=160 -> avg U=120, V=180.
        let src = [
            50_u8, 100, 60, 200, // row 0: Y0 U0 Y1 V0
            70, 140, 80, 160, // row 1: Y2 U1 Y3 V1
        ];
        let out = yuyv_to_i420(&src, 2, 2);
        assert_eq!(&out[0..4], &[50, 60, 70, 80]);
        assert_eq!(out[4], 120);
        assert_eq!(out[5], 180);
    }

    #[test]
    fn encoder_keeps_frames_unskipped_when_camera_runs_slower_than_rung() {
        let (w, h) = (640, 360);
        let mut enc = open_encoder(w, h, 500_000, 20).expect("open encoder");
        enc.force_intra_frame();
        let mut skips = 0;
        let mut keys = 0;
        let mut total_bytes = 0usize;
        for i in 0..30_u64 {
            let mut yuv = vec![128_u8; w * h * 3 / 2];
            for y in 0..h {
                for x in 0..w {
                    yuv[y * w + x] = (((x + (i as usize) * 5) ^ (y + (i as usize) * 2)) % 180 + 40) as u8;
                }
            }
            let pic = YUVBuffer::from_vec(yuv, w, h);
            let bs = enc
                .encode_at(&pic, Timestamp::from_millis(i * 77))
                .expect("encode_at");
            if bs.frame_type() == FrameType::Skip {
                skips += 1;
            }
            if bs.frame_type() == FrameType::IDR {
                keys += 1;
            }
            total_bytes += bs.to_vec().len();
        }
        assert_eq!(skips, 0);
        assert!(keys >= 1);
        assert!(total_bytes > 50_000);
    }

    #[test]
    fn v4l_webcam_capture_big_and_reopen_works_when_present() {
        let path = Path::new("/dev/video0");
        if !path.exists() || !device_is_capture(path) {
            return;
        }
        let small = SendProfile {
            width: 640,
            height: 360,
            fps: 15,
            bitrate: 500_000,
            key_ms: 5_000,
            capture_big: false,
            rung: 3,
        };
        let big = SendProfile {
            width: 720,
            height: 360,
            fps: 20,
            bitrate: 300_000,
            key_ms: 10_000,
            capture_big: true,
            rung: 3,
        };
        let grab_sm = V4lGrab::open(path, &small).expect("open small");
        drop(grab_sm);
        let mut grab_bg = V4lGrab::open(path, &big).expect("open big");
        let buf = grab_bg.latest_frame().expect("big frame");
        assert!(!buf.is_empty());
        drop(grab_bg);

        let cap = CameraCapture::start(small, None).expect("start cap");
        cap.set_profile(big);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut got_frame = false;
        while std::time::Instant::now() < deadline {
            if let Some(ev) = cap.poll() {
                match ev {
                    CamEvent::Frame { data, .. } => {
                        assert!(!data.is_empty());
                        got_frame = true;
                        break;
                    }
                    CamEvent::Failed(e) => panic!("CameraCapture failed: {e}"),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(got_frame, "CameraCapture did not yield frame within 3s");
    }
}
