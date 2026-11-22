//! Native Windows Explorer file/folder pickers (`IFileOpenDialog` COM) and GDI
//! full-screen capture.

use std::path::PathBuf;

use crate::error::ShellError;

/// Glob patterns offered by the image picker (kept for settings tests).
pub const IMAGE_PATTERNS: &[&str] = &[
    "*.png", "*.jpg", "*.jpeg", "*.webp", "*.gif", "*.bmp", "*.PNG", "*.JPG", "*.JPEG",
];

/// Full-desktop capture tools (native Win32 GDI is used directly).
pub const SCREENSHOT_TOOLS: &[(&str, &[&str])] = &[("gdi", &[])];

/// Portal-shaped constant kept so call sites share a name with the Linux shell.
pub const PORTAL_FILECHOOSER_INTERFACE: &str = "windows.IFileOpenDialog";

/// `zenity --file-filter` text kept for shared test compatibility.
#[must_use]
pub fn zenity_filter(label: &str) -> String {
    format!("{label} | {}", IMAGE_PATTERNS.join(" "))
}

/// First absolute path in dialog stdout.
#[must_use]
pub fn parse_dialog_output(stdout: &str) -> Option<PathBuf> {
    let line = stdout.lines().next()?.trim();
    let path = PathBuf::from(line);
    path.is_absolute().then_some(path)
}

/// Absolute paths separated by newlines.
#[must_use]
pub fn parse_dialog_paths(stdout: &str) -> Vec<PathBuf> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .collect()
}

/// Image picker. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when COM `IFileOpenDialog` cannot be created.
pub fn pick_image(title: &str) -> Result<Option<PathBuf>, ShellError> {
    pick_files(title, true, false).map(|mut paths| paths.pop())
}

/// Image picker that allows several files.
///
/// # Errors
///
/// Returns [`ShellError`] when COM `IFileOpenDialog` cannot be created.
pub fn pick_images(title: &str) -> Result<Vec<PathBuf>, ShellError> {
    pick_files(title, true, true)
}

/// Any-file picker. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when COM `IFileOpenDialog` cannot be created.
pub fn pick_file(title: &str) -> Result<Option<PathBuf>, ShellError> {
    pick_files(title, false, false).map(|mut paths| paths.pop())
}

/// Folder picker. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when COM `IFileOpenDialog` cannot be created.
pub fn pick_folder(title: &str) -> Result<Option<PathBuf>, ShellError> {
    if cfg!(test) {
        return Ok(None);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = title;
        Err(ShellError::Bus(
            "IFileOpenDialog is only available on Windows".into(),
        ))
    }
    #[cfg(target_os = "windows")]
    {
        win_dialog::pick(title, false, false, true).map(|mut paths| paths.pop())
    }
}

/// Interactive capture fallback. On Windows the GPUI screenshot overlay handles
/// region selection directly; this falls back to full-screen GDI capture.
///
/// # Errors
///
/// Returns [`ShellError`] when GDI screen capture fails.
pub fn capture_interactive() -> Result<Vec<u8>, ShellError> {
    capture_fullscreen()
}

/// Full-screen PNG via Win32 GDI `BitBlt`.
///
/// # Errors
///
/// Returns [`ShellError`] when GDI capture or PNG encoding fails.
pub fn capture_fullscreen() -> Result<Vec<u8>, ShellError> {
    if cfg!(test) {
        return Ok(Vec::new());
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(ShellError::Bus(
            "GDI screen capture is only available on Windows".into(),
        ))
    }
    #[cfg(target_os = "windows")]
    {
        win_gdi::capture_primary_png()
    }
}

/// Alias of [`capture_fullscreen`].
///
/// # Errors
///
/// Returns [`ShellError`] when capture fails.
pub fn capture_screenshot() -> Result<Vec<u8>, ShellError> {
    capture_fullscreen()
}

fn pick_files(title: &str, images: bool, multiple: bool) -> Result<Vec<PathBuf>, ShellError> {
    if cfg!(test) {
        return Ok(Vec::new());
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (title, images, multiple);
        Err(ShellError::Bus(
            "IFileOpenDialog is only available on Windows".into(),
        ))
    }
    #[cfg(target_os = "windows")]
    {
        win_dialog::pick(title, images, multiple, false)
    }
}

#[cfg(target_os = "windows")]
mod win_dialog {
    use std::path::PathBuf;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST,
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

    use crate::error::ShellError;

    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn pick(
        title: &str,
        images: bool,
        multiple: bool,
        folder: bool,
    ) -> Result<Vec<PathBuf>, ShellError> {
        // Run COM IFileOpenDialog on a fresh STA thread so we never collide
        // with the caller's COM apartment mode (e.g. MTA worker threads).
        let title_owned = title.to_owned();
        let handle = std::thread::Builder::new()
            .name("zalo-file-dialog".into())
            .spawn(move || pick_on_sta(&title_owned, images, multiple, folder))
            .map_err(|err| ShellError::Bus(err.to_string()))?;
        handle
            .join()
            .map_err(|_| ShellError::Bus("file dialog thread panicked".into()))?
    }

    fn pick_on_sta(
        title: &str,
        images: bool,
        multiple: bool,
        folder: bool,
    ) -> Result<Vec<PathBuf>, ShellError> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let did_init = hr.is_ok();
            let res = run_dialog(title, images, multiple, folder);
            if did_init {
                CoUninitialize();
            }
            res
        }
    }

    unsafe fn run_dialog(
        title: &str,
        images: bool,
        multiple: bool,
        folder: bool,
    ) -> Result<Vec<PathBuf>, ShellError> {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
                .map_err(|err| ShellError::Bus(err.to_string()))?;

        let mut opts = dialog
            .GetOptions()
            .map_err(|err| ShellError::Bus(err.to_string()))?;
        opts |= FOS_FORCEFILESYSTEM;
        if folder {
            opts |= FOS_PICKFOLDERS;
        } else {
            opts |= FOS_FILEMUSTEXIST;
        }
        if multiple {
            opts |= FOS_ALLOWMULTISELECT;
        }
        dialog
            .SetOptions(opts)
            .map_err(|err| ShellError::Bus(err.to_string()))?;

        if !title.is_empty() {
            let title_w = wide_z(title);
            let _ = dialog.SetTitle(PCWSTR(title_w.as_ptr()));
        }

        if images && !folder {
            let filters = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Hình ảnh (*.png;*.jpg;*.webp;*.gif;*.bmp)"),
                    pszSpec: w!("*.png;*.jpg;*.jpeg;*.webp;*.gif;*.bmp"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("Tất cả tệp (*.*)"),
                    pszSpec: w!("*.*"),
                },
            ];
            let _ = dialog.SetFileTypes(&filters);
        }

        if let Err(err) = dialog.Show(None) {
            if err.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(Vec::new());
            }
            return Err(ShellError::Bus(err.to_string()));
        }

        let mut paths = Vec::new();
        if multiple {
            let results = dialog
                .GetResults()
                .map_err(|err| ShellError::Bus(err.to_string()))?;
            let count = results
                .GetCount()
                .map_err(|err| ShellError::Bus(err.to_string()))?;
            for idx in 0..count {
                if let Ok(item) = results.GetItemAt(idx) {
                    if let Some(path) = shell_item_path(&item) {
                        paths.push(path);
                    }
                }
            }
        } else if let Ok(item) = dialog.GetResult() {
            if let Some(path) = shell_item_path(&item) {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    unsafe fn shell_item_path(item: &IShellItem) -> Option<PathBuf> {
        let pwstr = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = pwstr.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(pwstr.0.cast()));
        path
    }
}

#[cfg(target_os = "windows")]
mod win_gdi {
    use std::ffi::c_void;
    use std::io::Cursor;

    use image::RgbaImage;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    use crate::error::ShellError;

    pub(super) fn capture_primary_png() -> Result<Vec<u8>, ShellError> {
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w <= 0 || h <= 0 {
                return Err(ShellError::Bus("invalid screen metrics".into()));
            }
            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            let bmp = CreateCompatibleBitmap(screen_dc, w, h);
            let old = SelectObject(mem_dc, bmp.into());
            let blit_res = BitBlt(mem_dc, 0, 0, w, h, Some(screen_dc), 0, 0, SRCCOPY);

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
            let mut bgra = vec![0_u8; (w as usize) * (h as usize) * 4];
            let lines = if blit_res.is_ok() {
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
                return Err(ShellError::Bus("GDI BitBlt/GetDIBits failed".into()));
            }

            // BGRA -> RGBA
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
                px[3] = 255;
            }
            let rgba = RgbaImage::from_raw(w as u32, h as u32, bgra)
                .ok_or_else(|| ShellError::Image("invalid screenshot buffer".into()))?;
            let mut png = Vec::new();
            image::DynamicImage::ImageRgba8(rgba)
                .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
                .map_err(|err| ShellError::Image(err.to_string()))?;
            Ok(png)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_dialog_paths_and_zenity_filter() {
        assert!(zenity_filter("Ảnh").contains("*.png"));
        let empty = parse_dialog_paths("   \n");
        assert!(empty.is_empty());
        assert!(pick_file("test").expect("test stub").is_none());
        assert!(pick_folder("test").expect("test stub").is_none());
    }
}
