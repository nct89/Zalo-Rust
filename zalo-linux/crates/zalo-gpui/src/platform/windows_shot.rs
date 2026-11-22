//! Native Windows GDI monitor & visible-window grabber for `screenshot_ui.rs`.
//!
//! Captures the active monitor containing the cursor via `MonitorFromPoint` +
//! `BitBlt` + `GetDIBits` and enumerates top-level DWM window bounds so the
//! shared GPUI screenshot crop overlay provides 1-click window snapping on
//! Windows with zero external binaries.

use std::ffi::c_void;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTOPRIMARY, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetCursorPos, GetSystemMetrics, GetWindowRect, IsIconic, IsWindowVisible,
    SM_CXSCREEN, SM_CYSCREEN,
};

use crate::screenshot_ui::CapturedMonitor;

/// Captures the active monitor containing the cursor (or primary monitor) at
/// native 1:1 resolution, plus visible top-level window rectangles in
/// monitor-local pixels.
pub(crate) fn grab_active_monitor() -> Result<CapturedMonitor, String> {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let (mx, my, mw, mh) = if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            let r = mi.rcMonitor;
            (r.left, r.top, r.right - r.left, r.bottom - r.top)
        } else {
            (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
        };

        if mw <= 0 || mh <= 0 {
            return Err("Không xác định được kích thước màn hình Windows.".into());
        }

        let windows = collect_visible_windows(mx, my, mw, mh);

        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let bmp = CreateCompatibleBitmap(screen_dc, mw, mh);
        let old = SelectObject(mem_dc, bmp.into());
        let ok = BitBlt(mem_dc, 0, 0, mw, mh, Some(screen_dc), mx, my, SRCCOPY).is_ok();

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: mw,
                biHeight: -mh,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![255_u8; (mw as usize) * (mh as usize) * 4];
        let lines = if ok {
            GetDIBits(
                mem_dc,
                bmp,
                0,
                mh as u32,
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
            return Err("Không chụp được màn hình Windows (GDI BitBlt thất bại).".into());
        }

        for px in bgra.chunks_exact_mut(4) {
            px[3] = 255;
        }

        Ok(CapturedMonitor {
            x: mx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
            y: my.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
            width: mw as u32,
            height: mh as u32,
            bgra,
            windows,
        })
    }
}

struct EnumCtx {
    mx: i32,
    my: i32,
    mw: i32,
    mh: i32,
    out: Vec<(f32, f32, f32, f32)>,
}

fn collect_visible_windows(mx: i32, my: i32, mw: i32, mh: i32) -> Vec<(f32, f32, f32, f32)> {
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut EnumCtx);
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return BOOL(1);
        }
        // Skip UWP cloaked / virtual-desktop-hidden windows.
        let mut cloaked = 0_u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return BOOL(1);
        }

        let mut rect = RECT::default();
        let got_rect = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok()
            || GetWindowRect(hwnd, &mut rect).is_ok();
        if !got_rect {
            return BOOL(1);
        }

        let l = (rect.left - ctx.mx).max(0);
        let t = (rect.top - ctx.my).max(0);
        let r = (rect.right - ctx.mx).min(ctx.mw);
        let b = (rect.bottom - ctx.my).min(ctx.mh);
        let w = r - l;
        let h = b - t;
        // Ignore tiny helper windows or full-desktop shells.
        if w >= 48 && h >= 48 && !(w >= ctx.mw - 4 && h >= ctx.mh - 4) {
            ctx.out.push((l as f32, t as f32, r as f32, b as f32));
        }
        BOOL(1)
    }

    let mut ctx = EnumCtx {
        mx,
        my,
        mw,
        mh,
        out: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM((&mut ctx as *mut EnumCtx) as isize));
    }
    ctx.out
}
