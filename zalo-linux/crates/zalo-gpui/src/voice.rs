//! Microphone voice recording (`zcall-audio` `CardMic` + pure-Rust `rusty_mp3`) and `send_voice`.

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use gpui::{
    div, hsla, px, rgb, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};

use crate::bridge::LiveAction;
use crate::color::{str_vi, token_rgb};
use crate::data::{self, ChatConv, ChatMsg};
use crate::fonts::FAMILY_SEMIBOLD;
use crate::media::MsgKind;
use crate::shell::ShellView;

/// Minimum length accepted for a voice note (matches `zalo-gui`).
pub(crate) const MIN_VOICE_MS: u64 = 400;
/// Auto-stop ceiling for a single recording.
pub(crate) const MAX_VOICE_SECS: u64 = 60;
const PCM_BYTES_PER_SEC: u64 = 32_000;

/// Active mic recording handle held by [`ShellView`].
pub(crate) struct VoiceRecorder {
    /// Timestamp when capture started.
    pub started_at: Instant,
    /// Shared flag to stop capture and encode (unless `cancel` is also set).
    pub stop: Arc<AtomicBool>,
    /// Shared flag to discard captured PCM.
    pub cancel: Arc<AtomicBool>,
}

/// Duration in ms for 16 kHz mono s16le PCM.
#[must_use]
pub(crate) fn pcm_duration_ms(pcm_bytes: usize) -> u64 {
    (pcm_bytes as u64).saturating_mul(1000) / PCM_BYTES_PER_SEC
}

/// Validates PCM length and returns duration in ms or a Vietnamese error.
pub(crate) fn validate_pcm_len(pcm_bytes: usize) -> Result<u64, String> {
    let ms = pcm_duration_ms(pcm_bytes);
    if ms < MIN_VOICE_MS {
        return Err("Ghi âm quá ngắn (< 0,4 giây)".to_owned());
    }
    Ok(ms)
}

/// Encodes 16 kHz mono `s16le` PCM into MP3 (`32 kbps`) in-process via pure-Rust `rusty_mp3`
/// (zero `ffmpeg` dependency).
pub(crate) fn pcm_s16le_16k_to_mp3(pcm: &[u8]) -> Result<Vec<u8>, String> {
    if pcm.len() < 2 {
        return Err("dữ liệu PCM rỗng".to_owned());
    }
    let mut encoder = rusty_mp3::Mp3Encoder::new(rusty_mp3::Mp3EncoderConfig {
        bitrate_kbps: 32,
        vbr_quality: None,
    });
    let aligned = &pcm[..pcm.len() & !1];
    encoder
        .push_pcm_s16le(aligned, 1, 16_000)
        .map_err(|e| format!("lỗi mã hóa MP3: {e:?}"))?;
    encoder.finish();
    let mut mp3 = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(pkt) => mp3.extend_from_slice(&pkt),
            Err(rusty_mp3::Error::Again | rusty_mp3::Error::Eof) => break,
            Err(e) => return Err(format!("lỗi xuất khung MP3: {e:?}")),
        }
    }
    if mp3.is_empty() {
        return Err("không tạo được dữ liệu MP3".to_owned());
    }
    Ok(mp3)
}

/// Stores recorded MP3 locally so the outgoing bubble plays immediately.
pub(crate) fn save_local_voice(client_id: u64, mp3: &[u8]) -> Option<PathBuf> {
    if cfg!(test) || mp3.is_empty() {
        return None;
    }
    let dir = crate::media::media_root()?.join("voice");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("rec-{client_id}.mp3"));
    std::fs::write(&path, mp3).ok()?;
    Some(path)
}

/// Captures mic frames until stopped/canceled or [`MAX_VOICE_SECS`], then encodes MP3.
/// Uses in-process `CardMic` (`libpulse-simple`) first so `pactl`/`parec` are not needed.
pub(crate) fn record_blocking(
    stop: &AtomicBool,
    cancel: &AtomicBool,
) -> Result<Option<(Vec<u8>, u64)>, String> {
    let mut pcm = Vec::new();
    let started = Instant::now();
    let max = Duration::from_secs(MAX_VOICE_SECS);
    if let Ok(card) = zcall_audio::capture::CardMic::open(None) {
        while !stop.load(Ordering::Relaxed) && started.elapsed() < max {
            match card.read_frame() {
                Ok(samples) => {
                    for s in samples {
                        pcm.extend_from_slice(&s.to_le_bytes());
                    }
                }
                Err(_) => break,
            }
        }
    } else {
        let mut cap = zcall_audio::capture::MicCapture::from_mic(None, None, true)
            .or_else(|_| zcall_audio::capture::MicCapture::from_mic(None, None, false))
            .map_err(|e| format!("Không mở được mic ({e})"))?;
        while !stop.load(Ordering::Relaxed) && started.elapsed() < max {
            match cap.next_frame() {
                Some(frame) => pcm.extend_from_slice(&frame),
                None => break,
            }
        }
        cap.close();
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let duration_ms = validate_pcm_len(pcm.len())?;
    let mp3 = pcm_s16le_16k_to_mp3(&pcm).map_err(|e| format!("Không mã hóa được thoại ({e})"))?;
    Ok(Some((mp3, duration_ms)))
}

impl ShellView {
    /// Starts recording a voice message for the active conversation, or stops & sends if active.
    pub(crate) fn start_voice_record(&mut self, cx: &mut Context<Self>) {
        if self.voice_rec.is_some() {
            self.stop_voice_record(false, cx);
            return;
        }
        let Some(conv) = self.selected_conv() else {
            self.set_note("Chọn một hội thoại để ghi âm");
            return;
        };
        if let Some((_, handle)) = self.voice_playing.take() {
            handle.stop();
        }
        self.sticker_open = false;
        self.menu = None;
        self.rail_popup = None;

        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        self.voice_rec = Some(VoiceRecorder {
            started_at: Instant::now(),
            stop: stop.clone(),
            cancel: cancel.clone(),
        });
        self.note = None;

        // UI timer refresh so the elapsed clock ticks smoothly during capture.
        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(Duration::from_millis(250)).await;
            let keep_ticking = this
                .update(cx, |view, cx| {
                    if view.voice_rec.is_some() {
                        cx.notify();
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if !keep_ticking {
                break;
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { record_blocking(&stop, &cancel) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.voice_rec = None;
                match res {
                    Ok(Some((mp3, duration_ms))) => {
                        view.send_voice_now(Some(conv), mp3, duration_ms);
                    }
                    Ok(None) => view.set_note("Đã hủy ghi âm"),
                    Err(err) => view.set_note(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Signals the active recorder to finish (`canceled = false`) or discard (`canceled = true`).
    pub(crate) fn stop_voice_record(&mut self, canceled: bool, cx: &mut Context<Self>) {
        let Some(rec) = &self.voice_rec else {
            return;
        };
        if canceled {
            rec.cancel.store(true, Ordering::Relaxed);
        }
        rec.stop.store(true, Ordering::Relaxed);
        if canceled {
            self.set_note("Đang hủy ghi âm…");
        } else {
            self.set_note("Đang mã hóa tin nhắn thoại…");
        }
        cx.notify();
    }

    /// Inserts a local voice bubble and uploads via `LiveAction::SendVoice` when live.
    pub(crate) fn send_voice_now(
        &mut self,
        target: Option<ChatConv>,
        bytes: Vec<u8>,
        duration_ms: u64,
    ) {
        let Some(conv) = target.or_else(|| self.selected_conv()) else {
            self.set_note("Chọn một hội thoại để gửi");
            return;
        };
        let now = data::now_ms();
        let cli = now.max(1) as u64;
        let id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.saturating_add(1);
        let local_path = save_local_voice(cli, &bytes);
        let mut msg = ChatMsg::text_msg(
            id,
            conv.id,
            conv.thread_id.clone(),
            true,
            str_vi("STR_YOU", "Bạn").to_owned(),
            self.self_uid().unwrap_or_default().to_owned(),
            "[Tin nhắn thoại]".to_owned(),
            data::vn_hm(now),
            now,
            Some(cli as i64),
            None,
        );
        msg.kind = MsgKind::Voice;
        msg.file_name = "voice.mp3".to_owned();
        msg.file_size = duration_ms;
        msg.media_path = local_path;
        self.push_message(msg);
        if conv.live && self.has_session {
            self.maybe_live(LiveAction::SendVoice {
                client_id: cli,
                thread_id: conv.thread_id,
                is_group: conv.is_group,
                bytes,
                file_name: "voice.mp3".to_owned(),
            });
            self.set_note("Đang gửi tin nhắn thoại lên máy chủ…");
        } else {
            self.set_note("Hội thoại mẫu / chưa đăng nhập — thoại chỉ lưu trên máy này");
        }
    }

    /// Recording control bar shown above the composer while the microphone is live.
    pub(crate) fn voice_record_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let elapsed_ms = self
            .voice_rec
            .as_ref()
            .map(|r| u64::try_from(r.started_at.elapsed().as_millis()).unwrap_or(0))
            .unwrap_or(0);
        let elapsed = crate::links::clock(elapsed_ms);
        let bg = token_rgb("--surface-background-subtle", "#f6f8fb");
        let line = crate::color::border_subtle();
        let danger = token_rgb("--R60", "#d93025");
        let blue = token_rgb("--B60", "#005ae0");
        let ink = token_rgb("--text-primary", "#081b3a");
        div()
            .id("voice-record-bar")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(px(14.))
            .py(px(8.))
            .bg(rgb(bg))
            .border_t_1()
            .border_color(line)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(div().size(px(10.)).rounded(px(5.)).bg(rgb(danger)))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(ink))
                            .child(format!("Đang ghi âm • {elapsed} / 1:00")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("voice-cancel")
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .text_color(rgb(danger))
                            .hover(|s| s.bg(hsla(0., 0., 0., 0.06)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.stop_voice_record(true, cx);
                            }))
                            .child("Hủy"),
                    )
                    .child(
                        div()
                            .id("voice-send")
                            .px(px(12.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .bg(rgb(blue))
                            .text_size(px(12.5))
                            .font_family(FAMILY_SEMIBOLD)
                            .text_color(rgb(crate::color::on_accent_fg()))
                            .hover(|s| s.opacity(0.9))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.stop_voice_record(false, cx);
                            }))
                            .child("Gửi thoại"),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_duration_and_min_validation() {
        assert_eq!(pcm_duration_ms(0), 0);
        assert_eq!(pcm_duration_ms(32_000), 1000);
        assert!(validate_pcm_len(12_000).is_err());
        assert_eq!(validate_pcm_len(12_800), Ok(400));
        assert_eq!(validate_pcm_len(64_000), Ok(2000));
    }

    #[test]
    fn encodes_16k_mono_pcm_to_valid_mp3_in_process_without_ffmpeg() {
        // 0.6 s of 440 Hz tone at 16 kHz mono s16le.
        let mut pcm = Vec::with_capacity(16_000 * 2);
        for i in 0..9_600 {
            let s = (0.4 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin()
                * i16::MAX as f32) as i16;
            pcm.extend_from_slice(&s.to_le_bytes());
        }
        let mp3 = pcm_s16le_16k_to_mp3(&pcm).expect("in-process mp3 encode");
        assert!(mp3.len() > 500, "encoded mp3 too short: {}", mp3.len());
        // Verify round-trip decode succeeds.
        let mut dec = rusty_mp3::Mp3Decoder::new();
        dec.push(&mp3);
        dec.flush();
        let mut decoded_samples = 0usize;
        while let Ok(frame) = dec.next_frame() {
            assert_eq!(frame.sample_rate, 16_000);
            assert_eq!(frame.channels, 1);
            decoded_samples += frame.samples.len();
        }
        assert!(decoded_samples >= 8_000);
    }
}
