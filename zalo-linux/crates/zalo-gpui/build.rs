//! GPUI links `-lxcb` / `-lxkbcommon`. Fedora runtime packages ship
//! `libxcb.so.1` without the unversioned `libxcb.so` symlink (that lives in
//! `-devel`). Without sudo we alias the runtime SONAMEs for the linker.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let dir = out.join("sys-so");
    std::fs::create_dir_all(&dir).expect("sys-so dir");
    for (name, soname) in [
        ("libxcb.so", "libxcb.so.1"),
        ("libxkbcommon.so", "libxkbcommon.so.0"),
        ("libxkbcommon-x11.so", "libxkbcommon-x11.so.0"),
    ] {
        if let Some(src) = find_soname(soname) {
            let dst = dir.join(name);
            let _ = std::fs::remove_file(&dst);
            let _ = symlink(src, dst);
        }
    }
    println!("cargo:rustc-link-search=native={}", dir.display());
}

fn find_soname(soname: &str) -> Option<PathBuf> {
    const LIB_DIRS: &[&str] = &[
        "/usr/lib64",
        "/usr/lib",
        "/lib64",
        "/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib/aarch64-linux-gnu",
    ];
    for dir in LIB_DIRS {
        let path = Path::new(dir).join(soname);
        if path.exists() {
            return Some(path);
        }
    }
    None
}
