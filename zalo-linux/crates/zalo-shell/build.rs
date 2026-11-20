//! Embeds the Zalo logo when `assets/zalo-256.png` exists in the crate.
//! Without it the tray falls back to a plain badge.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(zalo_logo)");
    println!("cargo::rerun-if-changed=assets/zalo-256.png");
    if std::path::Path::new("assets/zalo-256.png").exists() {
        println!("cargo::rustc-cfg=zalo_logo");
    }
}
