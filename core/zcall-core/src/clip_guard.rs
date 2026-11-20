//! ClipGuard, ported from the reference call implementation (`audio-io.py`).
//!
//! It turns the OS microphone volume down when speech clips, and back up after
//! a calm period. The pactl reads/writes are left to the caller: `feed` returns
//! a step request and `step` computes the target dB from the current volume.

/// Clipped-sample threshold (full-scale flat top after resampling).
pub const CLIP: i32 = 31_000;

/// Clipped samples per window (0.5 %) that trigger a step.
pub const MIN_CLIPPED: u32 = 16;

/// One step, in dB.
pub const STEP_DB: f64 = 3.0;

/// Minimum time between steps, in seconds.
pub const STEP_SEC: f64 = 1.0;

/// Never go below this.
pub const FLOOR_DB: f64 = -24.0;

/// Frames (200 ms) per decision window.
pub const WINDOW: u32 = 10;

/// Seconds without clipping before raising again.
pub const RAISE_SEC: f64 = 10.0;

/// A peak at or above this resets the calm period (= 32768 * 10^(-9/20)).
pub const RAISE_PEAK: f64 = 11627.52;

/// A step the caller should perform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepKind {
    /// Turn down by STEP_DB (carries the clipped count for logging).
    Down(u32),
    /// Turn back up by STEP_DB.
    Up,
}

/// The OS-volume guard state machine.
pub struct ClipGuard {
    /// Whether the guard is active (a source and pactl exist).
    pub on: bool,
    /// The user moved the slider: no automatic step after it.
    pub manual: bool,
    /// The level the user had; we never go above it.
    pub user_db: Option<f64>,
    /// The level we last wrote.
    pub set_db: Option<f64>,
    /// Steps performed, for diagnostics.
    pub steps: u32,
    calm_since: f64,
    win_peak: i32,
    frames: u32,
    clipped: u32,
    last_step: f64,
}

impl ClipGuard {
    /// Builds a guard; `on` is whether pactl can act.
    pub fn new(on: bool) -> Self {
        Self {
            on,
            manual: false,
            user_db: None,
            set_db: None,
            steps: 0,
            calm_since: 0.0,
            win_peak: 0,
            frames: 0,
            clipped: 0,
            last_step: 0.0,
        }
    }

    /// Records that the user moved the volume slider.
    pub fn set_manual(&mut self, manual: bool) {
        self.manual = manual;
    }

    /// Sets the user's level.
    pub fn set_user_db(&mut self, db: Option<f64>) {
        self.user_db = db;
    }

    /// Sets the level we last wrote.
    pub fn set_set_db(&mut self, db: Option<f64>) {
        self.set_db = db;
    }

    /// Feeds one 320-sample frame at monotonic time `now` (seconds).
    ///
    /// Returns a step request once a 200 ms window demands one.
    pub fn feed(&mut self, pcm: &[i16], now: f64) -> Option<StepKind> {
        let clipped = pcm
            .iter()
            .filter(|&&v| i32::from(v) >= CLIP || i32::from(v) <= -CLIP)
            .count() as u32;
        self.clipped += clipped;
        // `i16::MIN` makes `abs` panic in a debug build. A full-scale mic
        // sample is that value, and the panic kills the capture thread.
        let peak = pcm.iter().map(|v| v.saturating_abs()).max().unwrap_or(0);
        self.win_peak = self.win_peak.max(i32::from(peak));
        self.frames += 1;
        if self.frames < WINDOW {
            return None;
        }
        let clipped = self.clipped;
        let loud = f64::from(self.win_peak) >= RAISE_PEAK;
        self.frames = 0;
        self.clipped = 0;
        self.win_peak = 0;
        if clipped > 0 || loud {
            self.calm_since = now;
        }
        if !self.on || self.manual {
            return None;
        }
        if clipped >= MIN_CLIPPED && now - self.last_step >= STEP_SEC {
            self.last_step = now;
            return Some(StepKind::Down(clipped));
        }
        if now - self.calm_since >= RAISE_SEC
            && self.user_db.is_some()
            && self.set_db.is_some()
            && self.set_db.unwrap() < self.user_db.unwrap() - 0.5
        {
            self.calm_since = now;
            return Some(StepKind::Up);
        }
        None
    }

    /// Computes the target dB for a step, given the current OS volume `cur_db`.
    ///
    /// Returns the new volume in dB to write, or `None` when no change is due.
    pub fn step(&mut self, kind: StepKind, cur_db: Option<f64>) -> Option<f64> {
        let cur = cur_db?;
        if let Some(set) = self.set_db {
            if (cur - set).abs() > 2.0 {
                // Someone else moved it: that is their level now.
                self.user_db = Some(cur);
                self.set_db = Some(cur);
                if matches!(kind, StepKind::Up) {
                    return None;
                }
            }
        }
        if self.user_db.is_none() {
            self.user_db = Some(cur);
        }
        let new = match kind {
            StepKind::Down(_) => {
                if cur <= FLOOR_DB + 0.01 {
                    return None;
                }
                (cur - STEP_DB).max(FLOOR_DB)
            }
            StepKind::Up => {
                let user = self.user_db.unwrap();
                if cur >= user - 0.01 {
                    return None;
                }
                (cur + STEP_DB).min(user)
            }
        };
        self.steps += 1;
        self.set_db = Some(new);
        Some(new)
    }
}

/// Converts dB to PulseAudio's raw 0..65536 volume (cubic scale).
pub fn db_to_raw(db: f64) -> u32 {
    (65536.0 * 10_f64.powf(db / 60.0)).round() as u32
}

/// Converts raw volume to percent (0..100).
pub fn raw_to_percent(raw: u32) -> u32 {
    (f64::from(raw) / 655.36).round() as u32
}

/// Linear Pulse volume for a 0..=100 slider.
///
/// `pactl` prints `raw / 65536` as the percent. 100% is raw 65536 (0 dB).
/// This is not the cubic dB scale: 40% is the clip-guard floor, not −9 dB.
pub fn percent_to_raw(percent: u8) -> u32 {
    let percent = u32::from(percent.min(100));
    (65536 * percent + 50) / 100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_down_when_clipped() {
        let mut guard = ClipGuard::new(true);
        guard.set_user_db(Some(0.0));
        guard.set_set_db(Some(0.0));
        let clipped = vec![32_000_i16; 320];
        let mut request = None;
        for i in 0..WINDOW {
            let now = if i == WINDOW - 1 { 1.0 } else { 0.0 };
            let r = guard.feed(&clipped, now);
            if i == WINDOW - 1 {
                request = r;
            } else {
                assert!(r.is_none());
            }
        }
        assert!(matches!(request, Some(StepKind::Down(_))));
        assert_eq!(guard.step(StepKind::Down(16), Some(0.0)), Some(-3.0));
        assert_eq!(guard.set_db, Some(-3.0));
    }

    #[test]
    fn raises_after_a_calm_period() {
        let mut guard = ClipGuard::new(true);
        guard.set_user_db(Some(0.0));
        guard.set_set_db(Some(-3.0));
        let quiet = vec![1000_i16; 320];
        let mut request = None;
        for i in 0..WINDOW {
            let now = if i == WINDOW - 1 { 11.0 } else { 0.0 };
            let r = guard.feed(&quiet, now);
            if i == WINDOW - 1 {
                request = r;
            }
        }
        assert_eq!(request, Some(StepKind::Up));
        assert_eq!(guard.step(StepKind::Up, Some(-3.0)), Some(0.0));
    }

    #[test]
    fn never_goes_below_the_floor() {
        let mut guard = ClipGuard::new(true);
        guard.set_user_db(Some(0.0));
        guard.set_set_db(Some(-23.0));
        assert_eq!(guard.step(StepKind::Down(16), Some(-23.0)), Some(-24.0));
    }

    #[test]
    fn db_to_raw_matches_the_cubic_scale() {
        assert_eq!(db_to_raw(0.0), 65536);
        assert_eq!(db_to_raw(-60.0), 6554);
        assert_eq!(raw_to_percent(65536), 100);
        assert_eq!(raw_to_percent(32768), 50);
    }

    #[test]
    fn slider_percent_is_the_linear_raw_scale() {
        assert_eq!(percent_to_raw(100), 65536);
        assert_eq!(percent_to_raw(50), 32768);
        assert_eq!(percent_to_raw(40), 26214);
        assert_eq!(percent_to_raw(0), 0);
        assert_eq!(percent_to_raw(200), 65536);
        assert_eq!(raw_to_percent(percent_to_raw(40)), 40);
        assert_eq!(raw_to_percent(percent_to_raw(100)), 100);
    }

    #[test]
    fn manual_slider_stops_os_steps() {
        let mut guard = ClipGuard::new(true);
        guard.set_user_db(Some(0.0));
        guard.set_set_db(Some(0.0));
        guard.set_manual(true);
        let clipped = vec![32_000_i16; 320];
        let mut request = None;
        for i in 0..WINDOW {
            let now = if i + 1 == WINDOW { 1.0 } else { 0.0 };
            request = guard.feed(&clipped, now);
        }
        assert!(request.is_none());
        assert_eq!(guard.steps, 0);
    }

    #[test]
    fn full_scale_negative_sample_does_not_panic() {
        let mut guard = ClipGuard::new(true);
        let frame = vec![i16::MIN; 320];
        let step = guard.feed(&frame, 0.0);
        assert!(step.is_none());
    }
}
