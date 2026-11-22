//! `NSStatusItem` menu and Dock badge. Compiled only on macOS.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use cocoa::base::{id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSData, NSString};
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel, BOOL, NO};
use objc::{class, msg_send, sel, sel_impl};

use crate::badge::ZALO_ICON_PNG;
use crate::tray::{record_tray_event, TrayEvent};

static STATUS_ITEM: AtomicUsize = AtomicUsize::new(0);

/// Create the menu-bar item. Safe to call once; later calls keep the existing item.
pub(crate) fn install() {
    unsafe {
        if STATUS_ITEM.load(Ordering::Acquire) != 0 {
            return;
        }
        let pool = NSAutoreleasePool::new(nil);
        let target_cls = menu_target_class();
        let target: id = msg_send![target_cls, new];
        let _: id = msg_send![target, retain];

        let bar: id = msg_send![class!(NSStatusBar), systemStatusBar];
        // `NSSquareStatusItemLength` is -2.
        let item: id = msg_send![bar, statusItemWithLength: -2.0_f64];
        let _: id = msg_send![item, retain];

        let button: id = msg_send![item, button];
        if let Some(png) = ZALO_ICON_PNG {
            if let Some(image) = image_from_png(png) {
                let _: () = msg_send![button, setImage: image];
            }
        }
        let tip: id = NSString::alloc(nil).init_str("Zalo");
        let _: () = msg_send![button, setToolTip: tip];

        let menu: id = msg_send![class!(NSMenu), new];
        let _: id = msg_send![menu, retain];
        add_item(menu, target, "Mở Zalo", sel!(onOpen:));
        add_item(menu, target, "Ẩn Zalo", sel!(onHide:));
        add_item(menu, target, "Tắt/Bật thông báo", sel!(onMute:));
        let separator: id = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: separator];
        add_item(menu, target, "Thoát Zalo", sel!(onQuit:));
        let _: () = msg_send![item, setMenu: menu];

        STATUS_ITEM.store(item as usize, Ordering::Release);
        std::mem::forget(pool);
    }
}

/// Set the Dock badge. An empty label clears it.
pub(crate) fn set_badge(label: &str) {
    unsafe {
        apply_badge_label(label);
    }
}

unsafe fn apply_badge_label(label: &str) {
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let dock: id = msg_send![app, dockTile];
        if label.is_empty() {
            let _: () = msg_send![dock, setBadgeLabel: nil];
        } else {
            let text: id = NSString::alloc(nil).init_str(&label);
            let _: () = msg_send![dock, setBadgeLabel: text];
        }
        let item = STATUS_ITEM.load(Ordering::Acquire) as id;
        if !item.is_null() {
            let button: id = msg_send![item, button];
            let tip = if label.is_empty() {
                "Zalo".to_owned()
            } else {
                format!("Zalo ({label})")
            };
            let text: id = NSString::alloc(nil).init_str(&tip);
            let _: () = msg_send![button, setToolTip: text];
        }
}

unsafe fn image_from_png(png: &[u8]) -> Option<id> {
    let data: id = NSData::dataWithBytes_length_(
        nil,
        png.as_ptr().cast::<c_void>(),
        png.len() as u64,
    );
    if data.is_null() {
        return None;
    }
    let image: id = msg_send![class!(NSImage), alloc];
    let image: id = msg_send![image, initWithData: data];
    if image.is_null() {
        return None;
    }
    let template: BOOL = NO;
    let _: () = msg_send![image, setTemplate: template];
    let size = cocoa::foundation::NSSize::new(18.0, 18.0);
    let _: () = msg_send![image, setSize: size];
    Some(image)
}

unsafe fn add_item(menu: id, target: id, title: &str, action: objc::runtime::Sel) {
    let label: id = NSString::alloc(nil).init_str(title);
    let empty: id = NSString::alloc(nil).init_str("");
    let item: id = msg_send![class!(NSMenuItem), alloc];
    let item: id = msg_send![item, initWithTitle: label action: action keyEquivalent: empty];
    let _: () = msg_send![item, setTarget: target];
    let _: () = msg_send![menu, addItem: item];
}

fn menu_target_class() -> &'static Class {
    use std::sync::OnceLock;
    static CLASS: OnceLock<&'static Class> = OnceLock::new();
    CLASS.get_or_init(|| {
        let mut decl = ClassDecl::new("ZaloRustStatusTarget", class!(NSObject))
            .expect("ZaloRustStatusTarget");
        unsafe {
            decl.add_method(
                sel!(onOpen:),
                on_open as extern "C" fn(&Object, Sel, id),
            );
            decl.add_method(
                sel!(onHide:),
                on_hide as extern "C" fn(&Object, Sel, id),
            );
            decl.add_method(
                sel!(onMute:),
                on_mute as extern "C" fn(&Object, Sel, id),
            );
            decl.add_method(
                sel!(onQuit:),
                on_quit as extern "C" fn(&Object, Sel, id),
            );
        }
        decl.register()
    })
}

extern "C" fn on_open(_: &Object, _: Sel, _: id) {
    record_tray_event(TrayEvent::Activate);
}

extern "C" fn on_hide(_: &Object, _: Sel, _: id) {
    record_tray_event(TrayEvent::Hide);
}

extern "C" fn on_mute(_: &Object, _: Sel, _: id) {
    record_tray_event(TrayEvent::ToggleMute);
}

extern "C" fn on_quit(_: &Object, _: Sel, _: id) {
    record_tray_event(TrayEvent::Quit);
}
