//! Shows what a manifest's detection rules find on this machine. Read-only:
//! nothing is downloaded or installed, and no elevation is needed.
//!
//!     cargo run -p redist-core --example detect -- e2e/manifest.json

use redist_core::detection::{Machine, applies_to, is_installed};
use redist_core::manifest::Manifest;
use redist_core::windows::WindowsMachine;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: detect <manifest.json>");
        std::process::exit(2);
    };
    let manifest = match std::fs::read(&path).map(|bytes| Manifest::parse(&bytes)) {
        Ok(Ok(manifest)) => manifest,
        Ok(Err(error)) => {
            eprintln!("{path}: {error}");
            std::process::exit(2);
        }
        Err(error) => {
            eprintln!("{path}: {error}");
            std::process::exit(2);
        }
    };
    let machine = WindowsMachine::new().expect("x64 or ARM64 Windows");
    println!("Windows: {:?}", machine.native_arch());
    for component in &manifest.components {
        let state = if !applies_to(component.arch, machine.native_arch()) {
            "not for this machine".to_owned()
        } else {
            match is_installed(component, &machine) {
                Ok(true) => "installed".to_owned(),
                Ok(false) => "missing".to_owned(),
                Err(error) => format!("cannot tell: {error}"),
            }
        };
        println!("  {:<26} {state}", component.id);
    }
}
