//! Small cross-platform OS helpers shared across Linux, macOS, and Windows:
//! opening URLs/folders, querying disk space, and local timezone offset.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

/// Opens a file, folder, or `https://` URL with the OS default handler.
///
/// Replaces hardcoded `xdg-open` calls so Linux, macOS, and Windows all work.
pub(crate) fn open_external(target: impl AsRef<OsStr>) -> io::Result<()> {
    let target = target.as_ref();
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::{w, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let wide: Vec<u16> = target.encode_wide().chain(std::iter::once(0)).collect();
        let ret = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if (ret.0 as isize) <= 32 {
            return Err(io::Error::other(format!(
                "ShellExecuteW failed (code {})",
                ret.0 as isize
            )));
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(target).spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(target).spawn()?;
        Ok(())
    }
}

/// Returns `(drive_or_mount, total_bytes, used_bytes, available_bytes)` on Windows
/// via `GetDiskFreeSpaceExW`.
#[cfg(target_os = "windows")]
pub(crate) fn disk_info_windows(path: &Path) -> Option<(String, u64, u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut probe = path.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    let wide: Vec<u16> = probe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free_avail = 0_u64;
    let mut total = 0_u64;
    let mut total_free = 0_u64;
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free_avail),
            Some(&mut total),
            Some(&mut total_free),
        )
        .ok()?;
    }
    let used = total.saturating_sub(total_free);
    let label = probe
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| probe.display().to_string());
    Some((label, total, used, free_avail))
}

/// Local timezone offset east of UTC, in seconds, via Win32 `GetTimeZoneInformation`.
#[cfg(target_os = "windows")]
pub(crate) fn local_tz_offset_secs_windows() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

    let mut tzi = TIME_ZONE_INFORMATION::default();
    let mode = unsafe { GetTimeZoneInformation(&mut tzi) };
    // Windows Bias is minutes west of UTC: UTC = local + bias => offset_east = -bias.
    let bias_minutes = match mode {
        1 => tzi.Bias.saturating_add(tzi.StandardBias),
        2 => tzi.Bias.saturating_add(tzi.DaylightBias),
        _ => tzi.Bias,
    };
    -i64::from(bias_minutes) * 60
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
fn _keep_path_import(_: &Path) {}
