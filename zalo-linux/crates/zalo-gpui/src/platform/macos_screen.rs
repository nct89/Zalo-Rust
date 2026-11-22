//! Screen sharing via AVFoundation (`ffmpeg -f avfoundation` capture screen).

use std::io::Read;
use std::process::{Child, Command, Stdio};
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

use crate::call_camera::{CamEvent, LocalPreview};
use crate::macos_media::{self, bgra_to_i420, downscale_bgra};

const WIDTH: usize = 1280;
const HEIGHT: usize = 720;

/// A screen or display the user can share.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShareTarget {
    /// First Capture Screen device.
    #[default]
    FullScreen,
    /// A named AVFoundation screen index.
    Region {
        /// Row label.
        label: String,
        /// Unused on macOS (the device index selects the display).
        x: i16,
        /// Unused on macOS.
        y: i16,
        /// Width hint. Zero means the encoder size.
        w: u16,
        /// Height hint.
        h: u16,
        /// AVFoundation device index.
        index: String,
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
            Self::FullScreen => "Toàn bộ màn hình hiện tại".into(),
            Self::Region { w, h, .. } if *w > 0 && *h > 0 => format!("Màn hình • {w}×{h}"),
            Self::Region { .. } => "Màn hình".into(),
        }
    }

    fn device_index(&self) -> String {
        match self {
            Self::FullScreen => macos_media::list_video_devices()
                .into_iter()
                .find(|device| device.is_screen())
                .map(|device| device.index)
                .unwrap_or_else(|| "1".to_owned()),
            Self::Region { index, .. } => index.clone(),
        }
    }
}

/// Desktop capture encoded on a background thread.
pub(crate) struct ScreenCapture {
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<CamEvent>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
}

impl ScreenCapture {
    /// Starts an AVFoundation screen grab.
    ///
    /// # Errors
    ///
    /// Returns when the thread cannot start.
    pub(crate) fn start(target: ShareTarget) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let force_key = Arc::new(AtomicBool::new(true));
        let child = Arc::new(Mutex::new(None));
        let preview = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::sync_channel(3);
        let stop_t = Arc::clone(&stop);
        let key_t = Arc::clone(&force_key);
        let child_t = Arc::clone(&child);
        let preview_t = Arc::clone(&preview);
        let handle = std::thread::Builder::new()
            .name("zcall-screen".into())
            .spawn(move || capture_loop(stop_t, key_t, child_t, tx, preview_t, target))
            .map_err(|err| format!("Không mở được chia sẻ màn hình: {err}"))?;
        Ok(Self {
            stop,
            force_key,
            child,
            handle: Some(handle),
            rx,
            preview,
        })
    }

    /// Requests an IDR.
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
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Why sharing cannot start, before a thread is spawned.
#[must_use]
pub(crate) fn grab_unavailable() -> Option<String> {
    if macos_media::ffmpeg_available() {
        None
    } else {
        Some("Chia sẻ màn hình cần ffmpeg (brew install ffmpeg) và quyền Screen Recording.".into())
    }
}

/// Lists Capture Screen devices. Falls back to one full-screen row.
#[must_use]
pub(crate) fn list_share_targets() -> Vec<ShareTarget> {
    let screens: Vec<_> = macos_media::list_video_devices()
        .into_iter()
        .filter(|device| device.is_screen())
        .collect();
    if screens.is_empty() {
        return vec![ShareTarget::FullScreen];
    }
    screens
        .into_iter()
        .map(|device| ShareTarget::Region {
            label: device.name,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            index: device.index,
        })
        .collect()
}

fn capture_loop(
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
    tx: mpsc::SyncSender<CamEvent>,
    preview: Arc<Mutex<Option<LocalPreview>>>,
    target: ShareTarget,
) {
    let fail = |text: String| {
        let _ = tx.try_send(CamEvent::Failed(text));
    };
    if grab_unavailable().is_some() {
        fail("Cần ffmpeg (brew install ffmpeg) để chia sẻ màn hình.".into());
        return;
    }
    let index = target.device_index();
    let input = format!("{index}:none");
    let scale = format!("scale={WIDTH}:{HEIGHT}");
    let mut child = match Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "nobuffer",
            "-f",
            "avfoundation",
            "-framerate",
            "8",
            "-capture_cursor",
            "1",
            "-i",
        ])
        .arg(input)
        .arg("-vf")
        .arg(scale)
        .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            fail(format!("Không chạy được ffmpeg: {err}"));
            return;
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        fail("ffmpeg không có stdout".into());
        return;
    };
    if let Ok(mut guard) = child_slot.lock() {
        *guard = Some(child);
    }
    let Ok(mut encoder) = open_encoder() else {
        fail("Không mã hóa được màn hình.".into());
        return;
    };
    let started = Instant::now();
    let mut frame_no = 0_u32;
    while !stop.load(Ordering::Acquire) {
        let mut bgra = vec![0_u8; WIDTH * HEIGHT * 4];
        if !read_exact(&mut stdout, &mut bgra) {
            if !stop.load(Ordering::Acquire) {
                fail(
                    "Không đọc được màn hình. Hãy bật Screen Recording cho Zalo trong Cài đặt hệ thống."
                        .into(),
                );
            }
            return;
        }
        if frame_no % 2 == 0 {
            let (pw, ph) = (960_usize, 540_usize);
            let small = downscale_bgra(&bgra, WIDTH, HEIGHT, pw, ph);
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
        let i420 = bgra_to_i420(&bgra, WIDTH, HEIGHT);
        let picture = YUVBuffer::from_vec(i420, WIDTH, HEIGHT);
        let ts = Timestamp::from_millis(started.elapsed().as_millis() as u64);
        let Ok(bitstream) = encoder.encode_at(&picture, ts) else {
            continue;
        };
        if bitstream.frame_type() == FrameType::Skip || bitstream.to_vec().is_empty() {
            continue;
        }
        let data = bitstream.to_vec();
        let key = bitstream.frame_type() == FrameType::IDR;
        if let Err(TrySendError::Disconnected(_)) = tx.try_send(CamEvent::Frame { key, data }) {
            return;
        }
    }
}

fn open_encoder() -> Result<Encoder, openh264::Error> {
    let api = OpenH264API::from_source();
    let config = EncoderConfig::new()
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(1_200_000))
        .max_frame_rate(FrameRate::from_hz(8.0))
        .intra_frame_period(IntraFramePeriod::from_num_frames(0))
        .profile(Profile::Baseline)
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
