//! Call recordings storage (`~/Music/ZaloRecordings/`), `ddmmyyyy` naming,
//! date-filtered listing, and native stereo Ogg Opus playback for the left rail
//! `REC` panel.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Calendar date `(year, month 1..=12, day 1..=31)` in local time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CalDate {
    /// Four-digit year (e.g. `2026`).
    pub year: u32,
    /// Month `1..=12`.
    pub month: u8,
    /// Day of month `1..=31`.
    pub day: u8,
}

impl CalDate {
    /// Today's local calendar date.
    #[must_use]
    pub fn today() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self::from_unix_secs_local(secs)
    }

    /// Converts Unix milliseconds into a local [`CalDate`].
    #[must_use]
    pub fn from_unix_ms_local(at_ms: i64) -> Self {
        Self::from_unix_secs_local(at_ms.div_euclid(1_000))
    }

    /// Converts Unix seconds into a local [`CalDate`].
    #[must_use]
    pub fn from_unix_secs_local(secs: i64) -> Self {
        let shifted = secs.saturating_add(local_tz_offset_secs()).max(0) as u64;
        let (year, month, day, _, _, _) = utc_from_unix_secs(shifted);
        Self { year, month, day }
    }

    /// Formats as Vietnamese `DD/MM/YYYY`.
    #[must_use]
    pub fn format_dmy(self) -> String {
        format!("{:02}/{:02}/{:04}", self.day, self.month, self.year)
    }

    /// Parses a `DD/MM/YYYY` string back into a [`CalDate`].
    #[must_use]
    pub fn parse_dmy(s: &str) -> Option<Self> {
        let mut it = s.split('/');
        let d: u8 = it.next()?.parse().ok()?;
        let m: u8 = it.next()?.parse().ok()?;
        let y: u32 = it.next()?.parse().ok()?;
        if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        Some(Self {
            year: y,
            month: m,
            day: d,
        })
    }

    /// Days since Unix epoch (1970-01-01).
    #[must_use]
    pub fn to_days(self) -> i64 {
        days_from_civil(i64::from(self.year), u32::from(self.month), u32::from(self.day))
    }

    /// Returns the date `delta_days` earlier (or later if negative).
    #[must_use]
    pub fn sub_days(self, delta_days: i64) -> Self {
        let target = self.to_days().saturating_sub(delta_days).max(0) as u64;
        let (year, month, day, _, _, _) = utc_from_unix_secs(target * 86_400);
        Self { year, month, day }
    }

    /// Start of this local calendar day (`00:00:00.000`) in Unix milliseconds,
    /// matching Electron `compact3777.js::__verifyDate`.
    #[must_use]
    pub fn start_of_day_ms_local(self) -> i64 {
        let utc_secs = self.to_days().saturating_mul(86_400) - local_tz_offset_secs();
        utc_secs.saturating_mul(1_000)
    }

    /// End of this local calendar day (`23:59:59.999`) in Unix milliseconds,
    /// matching Electron `compact3777.js::__verifyDateEnd`.
    #[must_use]
    pub fn end_of_day_ms_local(self) -> i64 {
        self.start_of_day_ms_local().saturating_add(86_399_999)
    }
}

/// One cell in a 6×7 Sunday-first (`CN..T7`) calendar month grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalCell {
    /// Full date of this cell.
    pub date: CalDate,
    /// `true` when this cell belongs to the displayed `(year, month)`.
    pub in_month: bool,
}

/// Previous `(year, month)`.
#[must_use]
pub fn prev_month(year: u32, month: u8) -> (u32, u8) {
    if month <= 1 {
        (year.saturating_sub(1).max(1970), 12)
    } else {
        (year, month - 1)
    }
}

/// Next `(year, month)`.
#[must_use]
pub fn next_month(year: u32, month: u8) -> (u32, u8) {
    if month >= 12 {
        (year.saturating_add(1), 1)
    } else {
        (year, month + 1)
    }
}

/// Number of days in `(year, month)`.
#[must_use]
pub fn days_in_month(year: u32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Builds a 42-cell (6 rows × 7 cols) Sunday-first (`CN, T2, T3, T4, T5, T6, T7`)
/// calendar grid matching Zalo Electron/macOS.
#[must_use]
pub fn month_grid(year: u32, month: u8) -> [CalCell; 42] {
    let first = CalDate {
        year,
        month,
        day: 1,
    };
    // 1970-01-01 was Thursday (weekday 4 where Sunday = 0).
    let first_dow = (first.to_days() + 4).rem_euclid(7) as usize;
    let (py, pm) = prev_month(year, month);
    let prev_days = days_in_month(py, pm);
    let cur_days = days_in_month(year, month);
    let (ny, nm) = next_month(year, month);

    let mut cells = [CalCell {
        date: first,
        in_month: true,
    }; 42];
    for (idx, cell) in cells.iter_mut().enumerate() {
        if idx < first_dow {
            let day = prev_days - (first_dow - 1 - idx) as u8;
            *cell = CalCell {
                date: CalDate {
                    year: py,
                    month: pm,
                    day,
                },
                in_month: false,
            };
        } else if idx < first_dow + usize::from(cur_days) {
            let day = (idx - first_dow + 1) as u8;
            *cell = CalCell {
                date: CalDate { year, month, day },
                in_month: true,
            };
        } else {
            let day = (idx - first_dow - usize::from(cur_days) + 1) as u8;
            *cell = CalCell {
                date: CalDate {
                    year: ny,
                    month: nm,
                    day,
                },
                in_month: false,
            };
        }
    }
    cells
}

/// Cached local UTC offset in seconds (e.g. `+25200` for `+0700`).
fn local_tz_offset_secs() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let out = Command::new("date").arg("+%z").output().ok();
        let text = out
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
        if text.len() == 5 {
            let sign = if text.starts_with('-') { -1_i64 } else { 1_i64 };
            if let (Ok(hh), Ok(mm)) = (text[1..3].parse::<i64>(), text[3..5].parse::<i64>()) {
                return sign * (hh * 3600 + mm * 60);
            }
        }
        0
    })
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m_adj = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * m_adj + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Default directory where stereo Ogg Opus call recordings are saved.
#[must_use]
pub fn recordings_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join("Music").join("ZaloRecordings"))
        .or_else(|| {
            zalo_core::XdgDirs::from_env()
                .ok()
                .map(|x| x.data_home().join("zalo-rust").join("recordings"))
        })
        .unwrap_or_else(|| std::env::temp_dir().join("ZaloRecordings"))
}

/// Builds a new `.ogg` recording path with `ddmmyyyy` date format:
/// `ZaloCall_DDMMYYYY_HHMMSS_<peer>.ogg`.
#[must_use]
pub fn recording_file_path(peer_name: &str) -> PathBuf {
    let base = recordings_dir();
    let stamp = Command::new("date")
        .arg("+%d%m%Y_%H%M%S")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let (y, m, d, hh, mm, ss) = utc_from_unix_secs(secs);
            format!("{d:02}{m:02}{y:04}_{hh:02}{mm:02}{ss:02}")
        });
    let safe_peer: String = peer_name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .chars()
        .take(36)
        .collect();
    let filename = if safe_peer.is_empty() {
        format!("ZaloCall_{stamp}.ogg")
    } else {
        format!("ZaloCall_{stamp}_{safe_peer}.ogg")
    };
    base.join(filename)
}

/// One `.ogg` call recording discovered on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingEntry {
    /// Full file path.
    pub path: PathBuf,
    /// File basename (`ZaloCall_20112026_090130_Truong.ogg`).
    pub filename: String,
    /// Human-readable call title / peer name.
    pub peer: String,
    /// Display date formatted `DD/MM/YYYY` (also used for date filter chips).
    pub date_key: String,
    /// Display time formatted `HH:MM:SS`.
    pub time_label: String,
    /// Chronological sort key `(year, month, day, hour, minute, second)`.
    pub sort_key: (u32, u8, u8, u8, u8, u8),
    /// Exact Ogg Opus duration in seconds, when readable.
    pub duration_secs: Option<u64>,
    /// File size in bytes.
    pub bytes: u64,
}

/// Lists `.ogg` call recordings in `dir`, newest first.
#[must_use]
pub fn list_recordings_in(dir: &Path) -> Vec<RecordingEntry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for item in read_dir.flatten() {
        let path = item.path();
        let is_ogg = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ogg"));
        if !is_ogg || !path.is_file() {
            continue;
        }
        let filename = item.file_name().to_string_lossy().into_owned();
        let meta = item.metadata().ok();
        let bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let parsed = parse_recording_filename(&filename).unwrap_or_else(|| {
            let secs = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let (y, m, d, hh, mm, ss) = utc_from_unix_secs(secs);
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| filename.clone());
            ParsedMeta {
                peer: stem,
                date_key: format!("{d:02}/{m:02}/{y:04}"),
                time_label: format!("{hh:02}:{mm:02}:{ss:02}"),
                sort_key: (y, m, d, hh, mm, ss),
            }
        });
        let duration_secs = zcall_audio::ogg_opus_duration_secs(&path);
        entries.push(RecordingEntry {
            path,
            filename,
            peer: parsed.peer,
            date_key: parsed.date_key,
            time_label: parsed.time_label,
            sort_key: parsed.sort_key,
            duration_secs,
            bytes,
        });
    }
    entries.sort_by(|a, b| {
        b.sort_key
            .cmp(&a.sort_key)
            .then_with(|| b.filename.cmp(&a.filename))
    });
    entries
}

/// Lists `.ogg` call recordings in the default [`recordings_dir`].
#[must_use]
pub fn list_recordings() -> Vec<RecordingEntry> {
    list_recordings_in(&recordings_dir())
}

/// Distinct `DD/MM/YYYY` dates (newest first) paired with recording counts.
#[must_use]
pub fn available_dates(entries: &[RecordingEntry]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for entry in entries {
        if let Some(last) = out.last_mut().filter(|(d, _)| *d == entry.date_key) {
            last.1 += 1;
        } else if let Some(slot) = out.iter_mut().find(|(d, _)| *d == entry.date_key) {
            slot.1 += 1;
        } else {
            out.push((entry.date_key.clone(), 1));
        }
    }
    out
}

/// Formats seconds as `MM:SS` or `HH:MM:SS`.
#[must_use]
pub fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

/// Formats byte size compactly (`18 KB`, `1.4 MB`).
#[must_use]
pub fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

/// Opens the call recordings folder in the desktop file manager (creating it
/// first if it does not yet exist).
pub fn open_recordings_folder() {
    let dir = recordings_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    let _ = Command::new(opener)
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Opens a specific `.ogg` file with the system's default audio player.
pub fn open_recording_external(path: &Path) {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    let _ = Command::new(opener)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// In-app stereo Ogg Opus playback controller owned by `ShellView`.
#[derive(Default)]
pub struct RecordingPlayback {
    active: Arc<Mutex<Option<(PathBuf, Arc<AtomicBool>)>>>,
}

impl RecordingPlayback {
    /// Returns the path currently playing, if any.
    #[must_use]
    pub fn playing_path(&self) -> Option<PathBuf> {
        self.active
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|(p, _)| p.clone()))
    }

    /// Stops any active playback immediately.
    pub fn stop(&self) {
        if let Ok(mut slot) = self.active.lock() {
            if let Some((_, flag)) = slot.take() {
                flag.store(true, Ordering::Release);
            }
        }
    }

    /// Toggles playback for `path`: stops if already playing `path`, otherwise
    /// starts playing `path` in stereo on a background thread.
    pub fn toggle(&self, path: PathBuf) {
        if self.playing_path().as_ref() == Some(&path) {
            self.stop();
            return;
        }
        self.stop();
        let stop_flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut slot) = self.active.lock() {
            *slot = Some((path.clone(), Arc::clone(&stop_flag)));
        }
        let active_ref = Arc::clone(&self.active);
        let path_t = path.clone();
        let _ = std::thread::Builder::new()
            .name("zalo-rec-play".into())
            .spawn(move || {
                let _ = zcall_audio::play_ogg_opus(&path_t, &stop_flag);
                if let Ok(mut slot) = active_ref.lock() {
                    if slot
                        .as_ref()
                        .is_some_and(|(_, f)| Arc::ptr_eq(f, &stop_flag))
                    {
                        *slot = None;
                    }
                }
            });
    }
}

struct ParsedMeta {
    peer: String,
    date_key: String,
    time_label: String,
    sort_key: (u32, u8, u8, u8, u8, u8),
}

/// Parses `ZaloCall_DDMMYYYY_HHMMSS_<peer>.ogg` (and legacy `YYYYMMDD_HHMMSS`).
fn parse_recording_filename(filename: &str) -> Option<ParsedMeta> {
    let stem = filename
        .strip_suffix(".ogg")
        .or_else(|| filename.strip_suffix(".OGG"))?;
    let rest = stem.strip_prefix("ZaloCall_")?;
    let mut parts = rest.splitn(3, '_');
    let date_raw = parts.next()?;
    let time_raw = parts.next()?;
    if date_raw.len() != 8
        || time_raw.len() != 6
        || !date_raw.bytes().all(|b| b.is_ascii_digit())
        || !time_raw.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let a: u32 = date_raw[0..4].parse().ok()?;
    let b: u32 = date_raw[4..8].parse().ok()?;
    let (year, month, day) = if (2000..=2099).contains(&b) {
        // Preferred DDMMYYYY format (e.g. 20112026 -> 20/11/2026)
        let d: u8 = date_raw[0..2].parse().ok()?;
        let m: u8 = date_raw[2..4].parse().ok()?;
        (b, m, d)
    } else if (2000..=2099).contains(&a) {
        // Legacy YYYYMMDD format (e.g. 20261120 -> 20/11/2026)
        let m: u8 = date_raw[4..6].parse().ok()?;
        let d: u8 = date_raw[6..8].parse().ok()?;
        (a, m, d)
    } else {
        return None;
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let hh: u8 = time_raw[0..2].parse().ok()?;
    let mm: u8 = time_raw[2..4].parse().ok()?;
    let ss: u8 = time_raw[4..6].parse().ok()?;
    if hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    let peer = parts
        .next()
        .map(|s| s.replace('_', " ").trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Cuộc gọi Zalo".to_owned());
    Some(ParsedMeta {
        peer,
        date_key: format!("{day:02}/{month:02}/{year:04}"),
        time_label: format!("{hh:02}:{mm:02}:{ss:02}"),
        sort_key: (year, month, day, hh, mm, ss),
    })
}

/// Howard Hinnant civil-from-days UTC breakdown (zero external deps, zero unsafe).
fn utc_from_unix_secs(secs: u64) -> (u32, u8, u8, u8, u8, u8) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hh = (rem / 3600) as u8;
    let mm = ((rem % 3600) / 60) as u8;
    let ss = (rem % 60) as u8;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let year = (y + i64::from(m <= 2)) as u32;
    (year, m, d, hh, mm, ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_ddmmyyyy_recording_filename() {
        let path = recording_file_path("Trường / Test:01");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("ZaloCall_"));
        assert!(name.ends_with(".ogg"));
        let parsed = parse_recording_filename(&name).expect("generated name must parse");
        assert_eq!(parsed.date_key.len(), 10);
        assert_eq!(&parsed.date_key[2..3], "/");
        assert_eq!(&parsed.date_key[5..6], "/");
        assert!(parsed.sort_key.0 >= 2020);
    }

    #[test]
    fn parses_both_ddmmyyyy_and_legacy_yyyymmdd_into_dd_mm_yyyy() {
        let dmy = parse_recording_filename("ZaloCall_20112026_091542_Truong_Nguyen.ogg")
            .expect("ddmmyyyy");
        assert_eq!(dmy.date_key, "20/11/2026");
        assert_eq!(dmy.time_label, "09:15:42");
        assert_eq!(dmy.peer, "Truong Nguyen");
        assert_eq!(dmy.sort_key, (2026, 11, 20, 9, 15, 42));

        let ymd =
            parse_recording_filename("ZaloCall_20261119_230510_Nhom_Du_An.ogg").expect("yyyymmdd");
        assert_eq!(ymd.date_key, "19/11/2026");
        assert_eq!(ymd.time_label, "23:05:10");
        assert_eq!(ymd.peer, "Nhom Du An");
        assert_eq!(ymd.sort_key, (2026, 11, 19, 23, 5, 10));
        assert!(dmy.sort_key > ymd.sort_key);
    }

    #[test]
    fn november_2026_calendar_matches_zalo_electron_grid() {
        let grid = month_grid(2026, 11);
        // Nov 1, 2026 is Sunday (CN -> col 0); Nov 20 is Friday (T6 -> col 5, row 2 = idx 19).
        assert_eq!(
            grid[0],
            CalCell {
                date: CalDate {
                    year: 2026,
                    month: 11,
                    day: 1
                },
                in_month: true
            }
        );
        assert_eq!(grid[19].date, CalDate { year: 2026, month: 11, day: 20 });
        assert!(grid[19].in_month);
        assert_eq!(grid[29].date, CalDate { year: 2026, month: 11, day: 30 });
        assert!(grid[29].in_month);
        assert_eq!(grid[30].date, CalDate { year: 2026, month: 12, day: 1 });
        assert!(!grid[30].in_month);
        assert_eq!(grid[19].date.format_dmy(), "20/11/2026");
        assert_eq!(CalDate::parse_dmy("20/11/2026"), Some(grid[19].date));
    }
}
