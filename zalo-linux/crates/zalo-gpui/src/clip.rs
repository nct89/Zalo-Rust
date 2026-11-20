//! Clipboard writes that other apps can read.
//!
//! GPUI 0.2.2 on Linux drops image MIME types:
//! - On Wayland, it only offers text MIME types and claims the selection with
//!   the last *key-press* serial, so we hand the bytes to `wl-copy`.
//! - On X11, `X11Client::write_to_clipboard` calls `set_text(item.text().unwrap_or_default())`,
//!   which overwrites X11 `CLIPBOARD` with `""` for image items. On X11 we keep
//!   a lightweight `x11rb` selection-owner thread that serves `TARGETS`,
//!   `TIMESTAMP`, and the image MIME (`image/png` / `image/gif`, including `INCR`
//!   for large PNGs) directly until another app claims `CLIPBOARD`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{App, ClipboardItem, Image, ImageFormat};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    Property, PropertyNotifyEvent, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent,
    SelectionRequestEvent, Time, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    X11ClipAtoms: X11ClipAtomsCookie {
        CLIPBOARD,
        TARGETS,
        TIMESTAMP,
        ATOM,
        INCR,
        IMAGE_PNG: b"image/png",
        IMAGE_GIF: b"image/gif",
        IMAGE_JPEG: b"image/jpeg",
        IMAGE_WEBP: b"image/webp",
        IMAGE_SVG: b"image/svg+xml",
        IMAGE_BMP: b"image/bmp",
        IMAGE_TIFF: b"image/tiff",
    }
}

struct X11ClipState {
    mime_atom: Atom,
    bytes: Vec<u8>,
}

struct IncrTransfer {
    requestor: u32,
    property: Atom,
    target: Atom,
    data: Vec<u8>,
    offset: usize,
    chunk_size: usize,
}

struct X11ImageClip {
    conn: Arc<RustConnection>,
    win_id: u32,
    atoms: X11ClipAtoms,
    payload: Arc<Mutex<Option<X11ClipState>>>,
    stopped: Arc<AtomicBool>,
}

impl X11ImageClip {
    fn connect() -> Option<Self> {
        let (conn, screen_num) = RustConnection::connect(None).ok()?;
        let screen = conn.setup().roots.get(screen_num)?;
        let win_id = conn.generate_id().ok()?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win_id,
            screen.root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::COPY_FROM_PARENT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .ok()?;
        let atoms = X11ClipAtoms::new(&conn).ok()?.reply().ok()?;
        conn.flush().ok()?;

        let conn = Arc::new(conn);
        let payload = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));
        let clip = Self {
            conn: Arc::clone(&conn),
            win_id,
            atoms,
            payload: Arc::clone(&payload),
            stopped: Arc::clone(&stopped),
        };
        let _ = std::thread::Builder::new()
            .name("zalo-gpui-x11-clip".into())
            .spawn(move || {
                serve_x11_clipboard(&conn, win_id, atoms, &payload, &stopped);
            });
        Some(clip)
    }

    fn atom_for(&self, format: ImageFormat) -> Atom {
        match format {
            ImageFormat::Png => self.atoms.IMAGE_PNG,
            ImageFormat::Gif => self.atoms.IMAGE_GIF,
            ImageFormat::Jpeg => self.atoms.IMAGE_JPEG,
            ImageFormat::Webp => self.atoms.IMAGE_WEBP,
            ImageFormat::Svg => self.atoms.IMAGE_SVG,
            ImageFormat::Bmp => self.atoms.IMAGE_BMP,
            ImageFormat::Tiff => self.atoms.IMAGE_TIFF,
        }
    }

    fn write_image(&self, format: ImageFormat, bytes: &[u8]) -> bool {
        if self.stopped.load(Ordering::Relaxed) {
            return false;
        }
        let mime_atom = self.atom_for(format);
        let Ok(mut guard) = self.payload.lock() else {
            return false;
        };
        *guard = Some(X11ClipState {
            mime_atom,
            bytes: bytes.to_vec(),
        });
        drop(guard);
        if self
            .conn
            .set_selection_owner(self.win_id, self.atoms.CLIPBOARD, Time::CURRENT_TIME)
            .is_err()
        {
            return false;
        }
        self.conn.flush().is_ok()
    }
}

fn serve_x11_clipboard(
    conn: &RustConnection,
    win_id: u32,
    atoms: X11ClipAtoms,
    payload: &Mutex<Option<X11ClipState>>,
    stopped: &AtomicBool,
) {
    let mut incr: Vec<IncrTransfer> = Vec::new();
    while let Ok(event) = conn.wait_for_event() {
        match event {
            Event::DestroyNotify(ev) if ev.window == win_id => break,
            Event::DestroyNotify(ev) => {
                incr.retain(|t| t.requestor != ev.window);
            }
            Event::SelectionClear(ev) if ev.selection == atoms.CLIPBOARD => {
                let lost = conn
                    .get_selection_owner(atoms.CLIPBOARD)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .is_none_or(|r| r.owner != win_id);
                if lost {
                    if let Ok(mut guard) = payload.lock() {
                        *guard = None;
                    }
                    incr.clear();
                }
            }
            Event::SelectionRequest(ev) => {
                handle_selection_request(conn, &atoms, payload, &mut incr, &ev);
            }
            Event::PropertyNotify(ev) if ev.state == Property::DELETE => {
                handle_incr_step(conn, &mut incr, &ev);
            }
            _ => {}
        }
    }
    stopped.store(true, Ordering::Relaxed);
}

fn send_selection_notify(conn: &RustConnection, ev: &SelectionRequestEvent, property: Atom) {
    let _ = conn.send_event(
        false,
        ev.requestor,
        EventMask::NO_EVENT,
        SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: ev.sequence,
            time: ev.time,
            requestor: ev.requestor,
            selection: ev.selection,
            target: ev.target,
            property,
        },
    );
    let _ = conn.flush();
}

fn handle_selection_request(
    conn: &RustConnection,
    atoms: &X11ClipAtoms,
    payload: &Mutex<Option<X11ClipState>>,
    incr: &mut Vec<IncrTransfer>,
    ev: &SelectionRequestEvent,
) {
    let none_atom: Atom = x11rb::NONE;
    if ev.selection != atoms.CLIPBOARD {
        send_selection_notify(conn, ev, none_atom);
        return;
    }
    let prop = if ev.property == none_atom {
        ev.target
    } else {
        ev.property
    };
    let (mime_atom, bytes) = {
        let Ok(guard) = payload.lock() else {
            send_selection_notify(conn, ev, none_atom);
            return;
        };
        let Some(state) = guard.as_ref() else {
            send_selection_notify(conn, ev, none_atom);
            return;
        };
        if ev.target == state.mime_atom {
            (state.mime_atom, Some(state.bytes.clone()))
        } else {
            (state.mime_atom, None)
        }
    };

    if ev.target == atoms.TARGETS {
        let targets = [atoms.TARGETS, atoms.TIMESTAMP, mime_atom];
        if conn
            .change_property32(PropMode::REPLACE, ev.requestor, prop, atoms.ATOM, &targets)
            .is_ok()
        {
            send_selection_notify(conn, ev, prop);
            return;
        }
    } else if ev.target == atoms.TIMESTAMP {
        let ts = ev.time.max(1);
        if conn
            .change_property32(PropMode::REPLACE, ev.requestor, prop, atoms.TIMESTAMP, &[ts])
            .is_ok()
        {
            send_selection_notify(conn, ev, prop);
            return;
        }
    } else if let Some(data) = bytes {
        let chunk_limit = usize::from(conn.setup().maximum_request_length)
            .saturating_mul(4)
            .saturating_sub(1024)
            .clamp(4096, 262_144);
        if data.len() <= chunk_limit {
            if conn
                .change_property8(PropMode::REPLACE, ev.requestor, prop, mime_atom, &data)
                .is_ok()
            {
                send_selection_notify(conn, ev, prop);
                return;
            }
        } else {
            let total = u32::try_from(data.len()).unwrap_or(u32::MAX);
            let _ = conn.change_window_attributes(
                ev.requestor,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            );
            if conn
                .change_property32(PropMode::REPLACE, ev.requestor, prop, atoms.INCR, &[total])
                .is_ok()
            {
                incr.retain(|t| !(t.requestor == ev.requestor && t.property == prop));
                if incr.len() >= 8 {
                    incr.remove(0);
                }
                incr.push(IncrTransfer {
                    requestor: ev.requestor,
                    property: prop,
                    target: mime_atom,
                    data,
                    offset: 0,
                    chunk_size: chunk_limit,
                });
                send_selection_notify(conn, ev, prop);
                return;
            }
        }
    }
    send_selection_notify(conn, ev, none_atom);
}

fn handle_incr_step(conn: &RustConnection, incr: &mut Vec<IncrTransfer>, ev: &PropertyNotifyEvent) {
    let Some(pos) = incr
        .iter()
        .position(|t| t.requestor == ev.window && t.property == ev.atom)
    else {
        return;
    };
    let transfer = &mut incr[pos];
    if transfer.offset < transfer.data.len() {
        let end = (transfer.offset + transfer.chunk_size).min(transfer.data.len());
        let chunk = &transfer.data[transfer.offset..end];
        if conn
            .change_property8(
                PropMode::REPLACE,
                transfer.requestor,
                transfer.property,
                transfer.target,
                chunk,
            )
            .is_ok()
        {
            let _ = conn.flush();
            transfer.offset = end;
            return;
        }
    } else {
        let _ = conn.change_property8(
            PropMode::REPLACE,
            transfer.requestor,
            transfer.property,
            transfer.target,
            &[],
        );
        let _ = conn.flush();
    }
    incr.remove(pos);
}

static X11_CLIP: OnceLock<Option<X11ImageClip>> = OnceLock::new();

fn x11_has_display() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

fn x11_copy_image(format: ImageFormat, bytes: &[u8]) -> bool {
    if !x11_has_display() {
        return false;
    }
    let Some(clip) = X11_CLIP.get_or_init(X11ImageClip::connect).as_ref() else {
        return false;
    };
    clip.write_image(format, bytes)
}

fn spawn_pipe(program: &str, args: &[&str], bytes: &[u8]) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(bytes).is_ok());
    wrote && child.wait().is_ok_and(|s| s.success())
}

fn wl_copy(mime: &str, bytes: &[u8]) -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|d| d.is_empty()) {
        return false;
    }
    spawn_pipe("wl-copy", &["--type", mime], bytes)
}

fn xclip_copy(mime: &str, bytes: &[u8]) -> bool {
    if !x11_has_display() {
        return false;
    }
    spawn_pipe("xclip", &["-selection", "clipboard", "-t", mime, "-i"], bytes)
}

/// Decodes a cached media file into PNG or preserves PNG/GIF bytes for clipboard paste.
///
/// # Errors
///
/// Returns a Vietnamese error message when reading or decoding the image fails.
pub fn encode_image_file(path: &Path) -> Result<(ImageFormat, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Png) => Ok((ImageFormat::Png, bytes)),
        Ok(image::ImageFormat::Gif) => Ok((ImageFormat::Gif, bytes)),
        _ => {
            let mut png = Vec::new();
            let rgba = crate::media::decode_rgba(&bytes, 8192)
                .ok_or_else(|| "Không đọc được ảnh".to_owned())?;
            rgba.write_to(
                &mut std::io::Cursor::new(&mut png),
                image::ImageFormat::Png,
            )
            .map_err(|e| e.to_string())?;
            Ok((ImageFormat::Png, png))
        }
    }
}

/// Copy plain text.
pub fn copy_text(cx: &mut App, text: String) {
    wl_copy("text/plain;charset=utf-8", text.as_bytes());
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// Copy an encoded image (PNG / JPEG / GIF / WebP).
pub fn copy_image(cx: &mut App, format: ImageFormat, bytes: Vec<u8>) {
    if wl_copy(format.mime_type(), &bytes) {
        return;
    }
    if x11_copy_image(format, &bytes) || xclip_copy(format.mime_type(), &bytes) {
        // Never fall through to GPUI's X11 `write_to_clipboard` for images:
        // GPUI 0.2.2 replaces X11 `CLIPBOARD` with `""` for non-text items.
        return;
    }
    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_image_file_keeps_png_and_converts_jpeg_to_png() {
        let dir = std::env::temp_dir().join(format!("zalo-gpui-clip-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([12, 34, 56, 255]));
        let png_path = dir.join("sample.png");
        img.save_with_format(&png_path, image::ImageFormat::Png)
            .unwrap();
        let (fmt, png_bytes) = encode_image_file(&png_path).unwrap();
        assert_eq!(fmt, ImageFormat::Png);
        assert_eq!(image::guess_format(&png_bytes).unwrap(), image::ImageFormat::Png);

        let jpg_path = dir.join("sample.jpg");
        image::DynamicImage::ImageRgba8(img)
            .to_rgb8()
            .save_with_format(&jpg_path, image::ImageFormat::Jpeg)
            .unwrap();
        let (fmt2, converted) = encode_image_file(&jpg_path).unwrap();
        assert_eq!(fmt2, ImageFormat::Png);
        assert_eq!(image::guess_format(&converted).unwrap(), image::ImageFormat::Png);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "requires an active X11 DISPLAY; verifies small direct + large INCR clipboard reads"]
    fn x11_clipboard_serves_small_and_large_png() {
        let clip = X11ImageClip::connect().expect("connect X11 clipboard owner");
        let (reader, screen_num) = RustConnection::connect(None).expect("connect X11 reader");
        let root = reader.setup().roots[screen_num].root;
        let req_win = reader.generate_id().unwrap();
        reader
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                req_win,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::COPY_FROM_PARENT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .unwrap();
        let atoms = X11ClipAtoms::new(&reader).unwrap().reply().unwrap();
        let prop = reader.intern_atom(false, b"ZALO_CLIP_TEST").unwrap().reply().unwrap().atom;
        reader.flush().unwrap();

        let read_target = |target: Atom| -> (Atom, Vec<u8>) {
            reader.delete_property(req_win, prop).unwrap();
            reader
                .convert_selection(req_win, atoms.CLIPBOARD, target, prop, Time::CURRENT_TIME)
                .unwrap();
            reader.flush().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut incr_buf = Vec::new();
            let mut using_incr = false;
            loop {
                assert!(std::time::Instant::now() < deadline, "X11 clipboard read timed out");
                match reader.poll_for_event().unwrap() {
                    Some(Event::SelectionNotify(ev)) if ev.selection == atoms.CLIPBOARD => {
                        assert_ne!(ev.property, x11rb::NONE, "owner rejected target {target}");
                        let reply = reader
                            .get_property(true, req_win, prop, x11rb::protocol::xproto::AtomEnum::ANY, 0, u32::MAX)
                            .unwrap()
                            .reply()
                            .unwrap();
                        if reply.type_ == atoms.INCR {
                            using_incr = true;
                            incr_buf.clear();
                        } else {
                            return (reply.type_, reply.value);
                        }
                    }
                    Some(Event::PropertyNotify(ev))
                        if using_incr
                            && ev.window == req_win
                            && ev.atom == prop
                            && ev.state == Property::NEW_VALUE =>
                    {
                        let reply = reader
                            .get_property(true, req_win, prop, x11rb::protocol::xproto::AtomEnum::ANY, 0, u32::MAX)
                            .unwrap()
                            .reply()
                            .unwrap();
                        if reply.value.is_empty() {
                            return (reply.type_, incr_buf);
                        }
                        incr_buf.extend_from_slice(&reply.value);
                    }
                    Some(_) => {}
                    None => std::thread::sleep(std::time::Duration::from_millis(2)),
                }
            }
        };

        // 1. Small direct PNG
        let small = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        assert!(clip.write_image(ImageFormat::Png, &small));
        let (ty, raw_targets) = read_target(atoms.TARGETS);
        assert_eq!(ty, atoms.ATOM);
        let offered: Vec<u32> = raw_targets
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert!(offered.contains(&atoms.IMAGE_PNG));
        let (img_ty, got_small) = read_target(atoms.IMAGE_PNG);
        assert_eq!(img_ty, atoms.IMAGE_PNG);
        assert_eq!(got_small, small);

        // 2. Large (> 256 KiB) INCR PNG
        let large: Vec<u8> = (0..380_000_u32).map(|i| (i % 251) as u8).collect();
        assert!(clip.write_image(ImageFormat::Png, &large));
        let (large_ty, got_large) = read_target(atoms.IMAGE_PNG);
        assert_eq!(large_ty, atoms.IMAGE_PNG);
        assert_eq!(got_large, large);

        let _ = reader.destroy_window(req_win);
        let _ = clip.conn.destroy_window(clip.win_id);
        let _ = clip.conn.flush();
    }
}
