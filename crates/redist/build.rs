//! Embeds the exe's application manifest: run as administrator, and declare
//! Windows 10 and 11 support (`windows-compat.manifest` in redist-core).
//!
//! `rustc-link-arg-bins` reaches only the exe, so this crate's tests run
//! without elevation.

use std::env;
use std::path::Path;

fn main() {
    println!("cargo::rerun-if-changed=../redist-core/windows-compat.manifest");
    let windows = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if windows && msvc {
        let dir = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
        let manifest = Path::new(&dir).join(r"..\redist-core\windows-compat.manifest");
        println!("cargo::rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo::rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
        println!("cargo::rustc-link-arg-bins=/MANIFESTUAC:level='requireAdministrator'");
    }
}
