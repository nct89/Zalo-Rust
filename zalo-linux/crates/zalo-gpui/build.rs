//! GPUI links `-lxcb` / `-lxkbcommon`. Fedora runtime packages ship
//! `libxcb.so.1` without the unversioned `libxcb.so` symlink (that lives in
//! `-devel`). Without sudo we alias the runtime SONAMEs for the linker.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../../../Cargo.toml");
    println!("cargo:rerun-if-env-changed=ZALO_BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=VERSION");

    let raw_version = std::env::var("ZALO_BUILD_VERSION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("VERSION")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    let clean_version = raw_version.trim().trim_start_matches(['v', 'V']);
    let app_version = if clean_version.is_empty() {
        env!("CARGO_PKG_VERSION")
    } else {
        clean_version
    };
    println!("cargo:rustc-env=ZALO_APP_VERSION={app_version}");
    println!("cargo:rustc-env=ZALO_APP_TAG=v{app_version}");

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=ZALO_BUILD_PROFILE={profile}");

    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    println!("cargo:rustc-env=ZALO_GIT_SHA={git_sha}");

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
