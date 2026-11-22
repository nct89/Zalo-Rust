//! DirectShow camera via `ffmpeg -f dshow`, encoded with OpenH264.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Level,
    Profile, QpRange, RateControlMode, UsageType,
};
use openh264::formats::YUVBuffer;
use openh264::{OpenH264API, Timestamp};
use zcall_core::send_rate::Profile as SendProfile;

use crate::windows_media::{self, bgra_to_i420, configure_hidden_command, downscale_bgra};

/// One encoded access unit, or why capture stopped.
pub(crate) enum CamEvent {
    /// Annex-B. `key` is an IDR.
    Frame {
        /// This access unit is a key frame.
        key: bool,
        /// Annex-B bytes.
        data: Vec<u8>,
    },
    /// Capture cannot continue.
    Failed(String),
}

/// Uncompressed local preview.
pub(crate) struct LocalPreview {
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
    /// Packed BGRA.
    pub bgra: Arc<Vec<u8>>,
}

enum CamCtrl {
    Profile(SendProfile),
    ForceKey,
}

/// DirectShow camera encoded on a background thread.
pub(crate) struct CameraCapture {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<CamEvent>,
    ctrl_tx: Sender<CamCtrl>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
}

impl CameraCapture {
    /// Starts `ffmpeg -f dshow`. A missing camera or ffmpeg is reported as [`CamEvent::Failed`].
    ///
    /// # Errors
    ///
    /// Returns when the worker thread cannot be spawned.
    pub(crate) fn start(
        initial: SendProfile,
        preferred_device: Option<String>,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let preview = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::sync_channel(3);
        let (ctrl_tx, ctrl_rx) = mpsc::channel();
        let stop_t = Arc::clone(&stop);
        let child_t = Arc::clone(&child);
        let preview_t = Arc::clone(&preview);
        let handle = std::thread::Builder::new()
            .name("zcall-cam".into())
            .spawn(move || {
                capture_loop(
                    stop_t,
                    child_t,
                    tx,
                    ctrl_rx,
                    initial,
                    preferred_device,
                    preview_t,
                );
            })
            .map_err(|err| format!("Không mở được camera: {err}"))?;
        Ok(Self {
            stop,
            child,
            handle: Some(handle),
            rx,
            ctrl_tx,
            preview,
        })
    }

    /// Latest local preview.
    pub(crate) fn take_preview(&self) -> Option<LocalPreview> {
        self.preview.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Updates the encode profile. Size changes restart `ffmpeg`.
    pub(crate) fn set_profile(&self, profile: SendProfile) {
        let _ = self.ctrl_tx.send(CamCtrl::Profile(profile));
    }

    /// Requests an IDR on the next frame.
    pub(crate) fn request_key_frame(&self) {
        let _ = self.ctrl_tx.send(CamCtrl::ForceKey);
    }

    /// Next encoded frame, if any.
    pub(crate) fn poll(&self) -> Option<CamEvent> {
        self.rx.try_recv().ok()
    }

    /// Stops ffmpeg and joins the thread.
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
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

fn kill_child(slot: &Mutex<Option<Child>>) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(child) = guard.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        *guard = None;
    }
}

fn capture_loop(
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    tx: mpsc::SyncSender<CamEvent>,
    ctrl_rx: Receiver<CamCtrl>,
    mut profile: SendProfile,
    preferred: Option<String>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
) {
    let fail = |text: String| {
        let _ = tx.try_send(CamEvent::Failed(text));
    };
    if !windows_media::ffmpeg_available() {
        fail("Cần cài ffmpeg vào PATH (winget install Gyan.FFmpeg) để bật camera.".into());
        return;
    }
    let devices = windows_media::list_dshow_video_devices();
    let device = preferred
        .as_deref()
        .and_then(parse_camera_path)
        .or_else(|| devices.into_iter().next());
    let Some(device) = device else {
        fail("Không tìm thấy camera nào trên Windows.".into());
        return;
    };

    let mut force_key = true;
    let mut encoder: Option<Encoder> = None;
    let mut encoded_size = (0_u32, 0_u32);
    let mut encoded_kbps = 0_u32;
    let mut encoded_fps = 0_u8;
    let started = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let (w, h) = (even(profile.width), even(profile.height));
        let fps = profile.fps.clamp(10, 30);
        let Ok((mut proc_child, mut stdout)) = spawn_ffmpeg(&device, w, h, fps) else {
            fail("Không mở được camera DirectShow.".into());
            return;
        };
        if let Ok(mut guard) = child.lock() {
            *guard = Some(proc_child);
        } else {
            let _ = proc_child.kill();
            return;
        }
        let frame_bytes = (w as usize) * (h as usize) * 4;
        let mut bgra = vec![0_u8; frame_bytes];
        let mut frame_no = 0_u32;

        loop {
            if stop.load(Ordering::Acquire) {
                kill_child(&child);
                return;
            }
            let mut restart = false;
            while let Ok(cmd) = ctrl_rx.try_recv() {
                match cmd {
                    CamCtrl::ForceKey => force_key = true,
                    CamCtrl::Profile(next) => {
                        if even(next.width) != w || even(next.height) != h {
                            restart = true;
                        }
                        profile = next;
                    }
                }
            }
            if restart {
                kill_child(&child);
                break;
            }
            if !read_exact(&mut stdout, &mut bgra) {
                kill_child(&child);
                if !stop.load(Ordering::Acquire) {
                    fail(
                        "Camera đã dừng hoặc đang bị ứng dụng khác sử dụng. Hãy kiểm tra quyền Camera trong Windows Settings."
                            .into(),
                    );
                }
                return;
            }
            if frame_no % 2 == 0 {
                let (pw, ph) = preview_size(w as usize, h as usize);
                let small = downscale_bgra(&bgra, w as usize, h as usize, pw, ph);
                if let Ok(mut slot) = preview.lock() {
                    *slot = Some(LocalPreview {
                        w: pw as u32,
                        h: ph as u32,
                        bgra: Arc::new(small),
                    });
                }
            }
            frame_no = frame_no.wrapping_add(1);

            let want_size = (w, h);
            let want_kbps = profile.kbps.max(120);
            let want_fps = profile.fps.clamp(10, 30);
            if encoder.is_none()
                || encoded_size != want_size
                || encoded_kbps != want_kbps
                || encoded_fps != want_fps
            {
                match open_encoder(want_kbps, want_fps) {
                    Ok(enc) => {
                        encoder = Some(enc);
                        encoded_size = want_size;
                        encoded_kbps = want_kbps;
                        encoded_fps = want_fps;
                        force_key = true;
                    }
                    Err(err) => {
                        kill_child(&child);
                        fail(format!("Không khởi tạo được bộ mã hóa H.264: {err}"));
                        return;
                    }
                }
            }
            let Some(enc) = encoder.as_mut() else {
                continue;
            };
            if force_key {
                enc.force_intra_frame();
                force_key = false;
            }
            let i420 = bgra_to_i420(&bgra, w as usize, h as usize);
            let picture = YUVBuffer::from_vec(i420, w as usize, h as usize);
            let ts = Timestamp::from_millis(started.elapsed().as_millis() as u64);
            let Ok(bitstream) = enc.encode_at(&picture, ts) else {
                continue;
            };
            if bitstream.frame_type() == FrameType::Skip || bitstream.to_vec().is_empty() {
                continue;
            }
            let data = bitstream.to_vec();
            let key = bitstream.frame_type() == FrameType::IDR;
            if let Err(TrySendError::Disconnected(_)) = tx.try_send(CamEvent::Frame { key, data }) {
                kill_child(&child);
                return;
            }
        }
    }
}

fn spawn_ffmpeg(
    device_name: &str,
    w: u32,
    h: u32,
    fps: u8,
) -> Result<(Child, std::process::ChildStdout), std::io::Error> {
    let input = format!("video={device_name}");
    let scale = format!("scale={w}:{h}");
    let fps_str = fps.to_string();
    let mut cmd = Command::new("ffmpeg");
    configure_hidden_command(&mut cmd)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "nobuffer",
            "-f",
            "dshow",
            "-framerate",
            &fps_str,
            "-i",
        ])
        .arg(input)
        .arg("-vf")
        .arg(scale)
        .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("ffmpeg missing stdout"))?;
    Ok((child, stdout))
}

fn open_encoder(kbps: u32, fps: u8) -> Result<Encoder, openh264::Error> {
    let api = OpenH264API::from_source();
    let config = EncoderConfig::new()
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(kbps.saturating_mul(1_000)))
        .max_frame_rate(FrameRate::from_hz(f32::from(fps)))
        .intra_frame_period(IntraFramePeriod::from_num_frames(0))
        .profile(Profile::Baseline)
        .level(Level::Level_3_1)
        .complexity(Complexity::Low)
        .qp(QpRange::new(18, 42))
        .num_threads(2);
    Encoder::with_api_config(api, config)
}

fn read_exact(reader: &mut impl Read, buf: &mut [u8]) -> bool {
    let mut filled = 0;
    let deadline = Instant::now() + Duration::from_secs(4);
    while filled < buf.len() {
        if Instant::now() > deadline {
            return false;
        }
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return false,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    true
}

fn even(n: u32) -> u32 {
    (n.max(160)) & !1
}

fn preview_size(w: usize, h: usize) -> (usize, usize) {
    if w <= 320 {
        return (w, h);
    }
    let scale = 320.0 / (w as f32);
    (
        320,
        even((h as f32 * scale) as u32) as usize,
    )
}

/// `"Integrated Webcam"` → `Some("Integrated Webcam")`. `"Mặc định"` returns `None`.
#[must_use]
pub(crate) fn parse_camera_path(choice: &str) -> Option<String> {
    let trimmed = choice.trim();
    if trimmed.is_empty() || trimmed == "Mặc định" {
        return None;
    }
    let name = trimmed
        .split_once(" — ")
        .map(|(_, right)| right.trim())
        .unwrap_or(trimmed);
    (!name.is_empty()).then(|| name.to_owned())
}

/// Formats a camera label for Settings.
#[must_use]
pub(crate) fn format_camera_option(index: &str, name: &str) -> String {
    if index.is_empty() {
        name.to_owned()
    } else {
        format!("{index} — {name}")
    }
}

/// Camera names for Settings.
#[must_use]
pub(crate) fn camera_option_labels() -> Vec<String> {
    windows_media::list_dshow_video_devices()
        .into_iter()
        .enumerate()
        .map(|(i, name)| format_camera_option(&i.to_string(), &name))
        .collect()
}

/// Moves `preferred` to the front when it matches.
#[must_use]
pub(crate) fn order_video_devices(mut devices: Vec<String>, preferred: Option<&str>) -> Vec<String> {
    let Some(target) = preferred.and_then(parse_camera_path) else {
        return devices;
    };
    if let Some(pos) = devices.iter().position(|item| {
        parse_camera_path(item).as_deref() == Some(target.as_str()) || item == &target
    }) {
        let picked = devices.remove(pos);
        devices.insert(0, picked);
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_choice_extracts_dshow_name() {
        assert_eq!(parse_camera_path("Mặc định"), None);
        assert_eq!(
            parse_camera_path("0 — Integrated Webcam").as_deref(),
            Some("Integrated Webcam")
        );
        let ordered = order_video_devices(
            vec!["0 — USB Cam".into(), "1 — Integrated Webcam".into()],
            Some("1 — Integrated Webcam"),
        );
        assert_eq!(ordered[0], "1 — Integrated Webcam");
    }
}
