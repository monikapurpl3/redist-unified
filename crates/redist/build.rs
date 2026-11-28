//! Compiles the window (`ui/app.slint`), embeds the exes' application
//! manifest (run as administrator, and declare Windows 10 and 11 support:
//! `windows-compat.manifest` in redist-core), and gives them their icon
//! (`assets/redist.ico`).
//!
//! `rustc-link-arg-bins` reaches only the exes, so this crate's tests and
//! examples run without elevation. The icon also goes into the examples, so
//! the demo's window has it.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Fluent is Slint's Windows 11 style; it follows Windows' light or dark
    // mode.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("ui/app.slint compiles");

    println!("cargo::rerun-if-changed=../redist-core/windows-compat.manifest");
    println!("cargo::rerun-if-changed=assets/redist.ico");
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

        // link.exe takes a compiled resource file (.res) as an input, so the
        // icon needs no resource compiler: this writes the .res itself.
        let ico = fs::read(Path::new(&dir).join(r"assets\redist.ico")).expect("assets/redist.ico");
        let out = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
        let res = Path::new(&out).join("redist-icon.res");
        fs::write(&res, icon_resource(&ico)).expect("the icon's .res is written");
        println!("cargo::rustc-link-arg-bins={}", res.display());
        println!("cargo::rustc-link-arg-examples={}", res.display());
    }
}

/// Resource types.
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

/// The icon file as a .res: each image as an RT_ICON resource (1, 2, ...),
/// and the group that lists them as RT_GROUP_ICON 1. The first icon group is
/// the one Explorer shows for an exe; the window loads it too (`app.rs`).
fn icon_resource(ico: &[u8]) -> Vec<u8> {
    let u16_at = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([ico[at], ico[at + 1], ico[at + 2], ico[at + 3]]);
    assert!(
        ico.len() >= 6 && u16_at(0) == 0 && u16_at(2) == 1,
        "assets/redist.ico is not an icon file"
    );
    let count = u16_at(4);

    let mut res = Vec::new();
    // Every .res starts with an empty entry.
    entry(&mut res, 0, 0, 0, &[]);
    // GRPICONDIR: reserved, type 1 (icon), count; then one 14-byte
    // GRPICONDIRENTRY per image: the file's 16-byte ICONDIRENTRY with its
    // 4-byte file offset replaced by a 2-byte resource id.
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend(count.to_le_bytes());
    for n in 0..usize::from(count) {
        let at = 6 + 16 * n;
        let (size, offset) = (u32_at(at + 8) as usize, u32_at(at + 12) as usize);
        let id = n as u16 + 1;
        // MOVEABLE | DISCARDABLE, as rc.exe writes for icons.
        entry(&mut res, RT_ICON, id, 0x1010, &ico[offset..offset + size]);
        group.extend_from_slice(&ico[at..at + 12]);
        group.extend(id.to_le_bytes());
    }
    // MOVEABLE | PURE | DISCARDABLE.
    entry(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    res
}

/// One resource: a 32-byte RESOURCEHEADER with a numeric type and name, the
/// data, and padding to a 4-byte boundary. The language is neutral.
fn entry(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    res.extend((data.len() as u32).to_le_bytes()); // DataSize
    res.extend(32u32.to_le_bytes()); // HeaderSize
    res.extend(0xFFFFu16.to_le_bytes()); // TYPE, numeric
    res.extend(kind.to_le_bytes());
    res.extend(0xFFFFu16.to_le_bytes()); // NAME, numeric
    res.extend(name.to_le_bytes());
    res.extend(0u32.to_le_bytes()); // DataVersion
    res.extend(flags.to_le_bytes()); // MemoryFlags
    res.extend(0u16.to_le_bytes()); // LanguageId
    res.extend(0u32.to_le_bytes()); // Version
    res.extend(0u32.to_le_bytes()); // Characteristics
    res.extend_from_slice(data);
    while res.len() % 4 != 0 {
        res.push(0);
    }
}
