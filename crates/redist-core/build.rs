//! Embeds `windows-compat.manifest` in this crate's tests and examples, so
//! they see Windows as the client does. `rustc-link-arg` reaches only this
//! package's own executables, never the crates that depend on it.

use std::env;
use std::path::Path;

fn main() {
    println!("cargo::rerun-if-changed=windows-compat.manifest");
    let windows = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if windows && msvc {
        let dir = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
        let manifest = Path::new(&dir).join("windows-compat.manifest");
        println!("cargo::rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo::rustc-link-arg=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
