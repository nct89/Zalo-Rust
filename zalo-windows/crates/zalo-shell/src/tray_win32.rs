//! Native Win32 notification-area icon (`Shell_NotifyIconW`), dynamic unread
//! badge `HICON`, popup context menu, balloon notifications, and global hotkeys.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_USER, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DispatchMessageW, GetCursorPos, GetMessageW, PostMessageW,
    RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, HICON, ICONINFO, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL,
    WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use crate::badge::{badge_label, render_tray_rgba, zalo_icon_png};
use crate::desktop::{ShortcutEvent, ShortcutSpec};
use crate::error::ShellError;
use crate::tray::{record_tray_event, tray_muted, TrayEvent};

const TRAY_UID: u32 = 1;
const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
const WM_TRAY_UPDATE_BADGE: u32 = WM_APP + 2;
const WM_TRAY_SHOW_BALLOON: u32 = WM_APP + 3;
const WM_TRAY_SYNC_HOTKEYS: u32 = WM_APP + 4;
const NIN_BALLOONUSERCLICK_MSG: u32 = 0x0405;

const CMD_OPEN: u32 = 1001;
const CMD_HIDE: u32 = 1002;
const CMD_TOGGLE_MUTE: u32 = 1003;
const CMD_QUIT: u32 = 1004;

type HotkeyCallback = Arc<dyn Fn(ShortcutEvent) + Send + Sync>;

struct SharedState {
    hwnd: isize,
    unread: u32,
    pending_balloon: Option<(String, String)>,
    pending_hotkeys: Option<(Vec<ShortcutSpec>, HotkeyCallback)>,
    active_hotkeys: HashMap<i32, String>,
    hotkey_cb: Option<HotkeyCallback>,
    taskbar_created_msg: u32,
    current_icon: isize,
    installed_icon: bool,
}

fn shared() -> &'static Mutex<SharedState> {
    static STATE: OnceLock<Mutex<SharedState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(SharedState {
            hwnd: 0,
            unread: 0,
            pending_balloon: None,
            pending_hotkeys: None,
            active_hotkeys: HashMap::new(),
            hotkey_cb: None,
            taskbar_created_msg: 0,
            current_icon: 0,
            installed_icon: false,
        })
    })
}

fn started() -> &'static OnceLock<()> {
    static STARTED: OnceLock<()> = OnceLock::new();
    &STARTED
}

/// Start the background Win32 tray window & message loop (idempotent).
pub(crate) fn install() {
    started().get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("zalo-win32-tray".into())
            .spawn(run_message_loop);
    });
}

/// Update the tray icon badge count and tooltip.
pub(crate) fn set_unread(count: u32) {
    let hwnd = {
        let Ok(mut guard) = shared().lock() else {
            return;
        };
        guard.unread = count;
        guard.hwnd
    };
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut c_void)),
                WM_TRAY_UPDATE_BADGE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

/// Show a Windows notification-area balloon/toast on the tray icon.
pub(crate) fn show_balloon(title: &str, body: &str) -> Result<(), ShellError> {
    install();
    let hwnd = {
        let Ok(mut guard) = shared().lock() else {
            return Err(ShellError::Bus("tray lock poisoned".into()));
        };
        guard.pending_balloon = Some((title.to_owned(), body.to_owned()));
        guard.hwnd
    };
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut c_void)),
                WM_TRAY_SHOW_BALLOON,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    Ok(())
}

/// Register (or rebind) global Win32 hotkeys on the tray message thread.
pub(crate) fn register_hotkeys(specs: Vec<ShortcutSpec>, cb: HotkeyCallback) {
    install();
    let hwnd = {
        let Ok(mut guard) = shared().lock() else {
            return;
        };
        guard.pending_hotkeys = Some((specs, cb));
        guard.hwnd
    };
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut c_void)),
                WM_TRAY_SYNC_HOTKEYS,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

fn run_message_loop() {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let class_name = w!("ZaloRustWin32TrayWindow");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        // Regular hidden top-level window (not HWND_MESSAGE) so we receive
        // broadcast `TaskbarCreated` messages when Explorer restarts.
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("Zalo"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        ) else {
            return;
        };

        let taskbar_msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        {
            if let Ok(mut guard) = shared().lock() {
                guard.hwnd = hwnd.0 as isize;
                guard.taskbar_created_msg = taskbar_msg;
            }
        }

        add_or_update_icon(hwnd, true);
        apply_pending_hotkeys(hwnd);
        apply_pending_balloon(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        remove_icon(hwnd);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let taskbar_msg = shared()
        .lock()
        .map(|g| g.taskbar_created_msg)
        .unwrap_or(0);

    if taskbar_msg != 0 && msg == taskbar_msg {
        add_or_update_icon(hwnd, true);
        return LRESULT(0);
    }

    match msg {
        WM_TRAY_CALLBACK => {
            let event = (lparam.0 & 0xffff) as u32;
            match event {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_BALLOONUSERCLICK_MSG => {
                    record_tray_event(TrayEvent::Activate);
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    show_context_menu(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TRAY_UPDATE_BADGE => {
            add_or_update_icon(hwnd, false);
            LRESULT(0)
        }
        WM_TRAY_SHOW_BALLOON => {
            apply_pending_balloon(hwnd);
            LRESULT(0)
        }
        WM_TRAY_SYNC_HOTKEYS => {
            apply_pending_hotkeys(hwnd);
            LRESULT(0)
        }
        WM_HOTKEY => {
            let id = wparam.0 as i32;
            let hit = shared().lock().ok().and_then(|g| {
                let spec_id = g.active_hotkeys.get(&id)?.clone();
                let cb = g.hotkey_cb.clone()?;
                Some((spec_id, cb))
            });
            if let Some((spec_id, cb)) = hit {
                cb(ShortcutEvent::Activated(spec_id));
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_icon(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn add_or_update_icon(hwnd: HWND, force_add: bool) {
    let (count, prev_icon, already_added) = match shared().lock() {
        Ok(g) => (g.unread, g.current_icon, g.installed_icon),
        Err(_) => return,
    };

    let hicon = build_tray_hicon(count);
    let tip = if count == 0 {
        "Zalo".to_owned()
    } else {
        format!("Zalo ({} tin chưa đọc)", badge_label(count))
    };

    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_MESSAGE | NIF_TIP | if hicon.is_some() { NIF_ICON } else { Default::default() },
        uCallbackMessage: WM_TRAY_CALLBACK,
        hIcon: hicon.unwrap_or_default(),
        ..Default::default()
    };
    write_wide_fixed(&mut nid.szTip, &tip);

    let op = if force_add || !already_added {
        NIM_ADD
    } else {
        NIM_MODIFY
    };
    let ok = Shell_NotifyIconW(op, &nid).as_bool();
    if !ok && op == NIM_MODIFY {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }

    if let Ok(mut g) = shared().lock() {
        g.installed_icon = true;
        if let Some(new_icon) = hicon {
            g.current_icon = new_icon.0 as isize;
            if prev_icon != 0 {
                let _ = DestroyIcon(HICON(prev_icon as *mut c_void));
            }
        }
    }
}

unsafe fn remove_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    if let Ok(mut g) = shared().lock() {
        for id in g.active_hotkeys.keys().copied() {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
        g.active_hotkeys.clear();
        if g.current_icon != 0 {
            let _ = DestroyIcon(HICON(g.current_icon as *mut c_void));
            g.current_icon = 0;
        }
        g.installed_icon = false;
    }
}

unsafe fn apply_pending_balloon(hwnd: HWND) {
    let pending = shared().lock().ok().and_then(|mut g| g.pending_balloon.take());
    let Some((title, body)) = pending else {
        return;
    };
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_USER,
        ..Default::default()
    };
    write_wide_fixed(&mut nid.szInfoTitle, &title);
    write_wide_fixed(&mut nid.szInfo, &body);
    let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
}

unsafe fn show_context_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else {
        record_tray_event(TrayEvent::ContextMenu);
        return;
    };
    let muted = tray_muted();
    let mute_flags = if muted {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    };
    let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN as usize, w!("Mở Zalo"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_HIDE as usize, w!("Thu gọn xuống khay"));
    let _ = AppendMenuW(menu, mute_flags, CMD_TOGGLE_MUTE as usize, w!("Tắt thông báo"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT as usize, w!("Thoát Zalo"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let chosen = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_RETURNCMD,
        pt.x,
        pt.y,
        Some(0),
        hwnd,
        None,
    )
    .0 as u32;
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    match chosen {
        CMD_OPEN => record_tray_event(TrayEvent::Activate),
        CMD_HIDE => record_tray_event(TrayEvent::Hide),
        CMD_TOGGLE_MUTE => record_tray_event(TrayEvent::ToggleMute),
        CMD_QUIT => record_tray_event(TrayEvent::Quit),
        _ => {}
    }
}

unsafe fn apply_pending_hotkeys(hwnd: HWND) {
    let pending = shared().lock().ok().and_then(|mut g| g.pending_hotkeys.take());
    let Some((specs, cb)) = pending else {
        return;
    };

    if let Ok(mut g) = shared().lock() {
        for id in g.active_hotkeys.keys().copied() {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
        g.active_hotkeys.clear();
        g.hotkey_cb = Some(Arc::clone(&cb));
    }

    let mut bound = Vec::new();
    let mut next_id: i32 = 1;
    for spec in specs {
        let Some((mods, vk)) = parse_trigger(&spec.trigger) else {
            continue;
        };
        if RegisterHotKey(Some(hwnd), next_id, mods | MOD_NOREPEAT, vk).is_ok() {
            if let Ok(mut g) = shared().lock() {
                g.active_hotkeys.insert(next_id, spec.id.clone());
            }
            bound.push((spec.id, spec.trigger));
            next_id += 1;
        }
    }

    if bound.is_empty() {
        cb(ShortcutEvent::Unavailable(
            "Không đăng ký được phím tắt toàn cục (có thể trùng với ứng dụng khác).".into(),
        ));
    } else {
        cb(ShortcutEvent::Bound(bound));
    }
}

fn parse_trigger(trigger: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key_token: Option<&str> = None;
    for part in trigger.split('+').map(str::trim).filter(|s| !s.is_empty()) {
        match part.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => mods |= MOD_CONTROL,
            "ALT" => mods |= MOD_ALT,
            "SHIFT" => mods |= MOD_SHIFT,
            "SUPER" | "WIN" | "META" => mods |= MOD_WIN,
            _ => key_token = Some(part),
        }
    }
    let token = key_token?.to_ascii_uppercase();
    let vk = if token.len() == 1 {
        let b = token.as_bytes()[0];
        if b.is_ascii_alphanumeric() {
            u32::from(b)
        } else {
            return None;
        }
    } else if let Some(num) = token.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&num) {
            0x70 + (num - 1)
        } else {
            return None;
        }
    } else {
        match token.as_str() {
            "PRINTSCREEN" | "PRINT" => 0x2C,
            "SPACE" => 0x20,
            _ => return None,
        }
    };
    Some((mods, vk))
}

unsafe fn build_tray_hicon(unread: u32) -> Option<HICON> {
    let base = zalo_icon_png(32).ok()?;
    let rgba = render_tray_rgba(&base, unread).ok()?;
    let w = rgba.width() as i32;
    let h = rgba.height() as i32;
    if w <= 0 || h <= 0 {
        return None;
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let hdc = GetDC(None);
    let mut bits: *mut c_void = std::ptr::null_mut();
    let color_bmp: HBITMAP = match CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(bmp) => bmp,
        Err(_) => {
            let _ = ReleaseDC(None, hdc);
            return None;
        }
    };
    let _ = ReleaseDC(None, hdc);

    if bits.is_null() {
        let _ = DeleteObject(color_bmp.into());
        return None;
    }

    let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
    for (src_px, dst_px) in rgba.as_raw().chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        // RGBA -> BGRA
        dst_px[0] = src_px[2];
        dst_px[1] = src_px[1];
        dst_px[2] = src_px[0];
        dst_px[3] = src_px[3];
    }

    let mask_bmp = CreateBitmap(w, h, 1, 1, None);
    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask_bmp,
        hbmColor: color_bmp,
    };
    let hicon = CreateIconIndirect(&info).ok();
    let _ = DeleteObject(color_bmp.into());
    let _ = DeleteObject(mask_bmp.into());
    hicon
}

fn write_wide_fixed(dst: &mut [u16], text: &str) {
    dst.fill(0);
    if dst.is_empty() {
        return;
    }
    let max_units = dst.len() - 1;
    for (slot, unit) in dst[..max_units].iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
    }
}
