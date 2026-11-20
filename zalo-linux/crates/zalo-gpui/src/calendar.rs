//! Calendar date arithmetic (`DD/MM/YYYY`) and 6×7 Sunday-first month grid
//! used by the in-chat message date-range search picker.

use std::process::Command;
use std::sync::OnceLock;
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

    /// Days since Unix epoch (1970-01-01).
    #[must_use]
    pub fn to_days(self) -> i64 {
        days_from_civil(
            i64::from(self.year),
            u32::from(self.month),
            u32::from(self.day),
        )
    }

    /// Returns the date `delta_days` earlier (or later if negative).
    #[must_use]
    pub fn sub_days(self, delta_days: i64) -> Self {
        let target = self.to_days().saturating_sub(delta_days).max(0) as u64;
        let (year, month, day, _, _, _) = utc_from_unix_secs(target * 86_400);
        Self { year, month, day }
    }

    /// Start of this local calendar day (`00:00:00.000`) in Unix milliseconds.
    #[must_use]
    pub fn start_of_day_ms_local(self) -> i64 {
        let utc_secs = self.to_days().saturating_mul(86_400) - local_tz_offset_secs();
        utc_secs.saturating_mul(1_000)
    }

    /// End of this local calendar day (`23:59:59.999`) in Unix milliseconds.
    #[must_use]
    pub fn end_of_day_ms_local(self) -> i64 {
        self.start_of_day_ms_local().saturating_add(86_399_999)
    }
}

/// One cell in a 6×7 Sunday-first (`Sun..Sat`) calendar month grid.
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

/// Builds a 42-cell (6 rows × 7 cols) Sunday-first (`Sun, Mon, Tue, Wed, Thu, Fri, Sat`)
/// calendar grid matching Zalo Electron/macOS.
#[must_use]
pub fn month_grid(year: u32, month: u8) -> [CalCell; 42] {
    let first = CalDate {
        year,
        month,
        day: 1,
    };
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
