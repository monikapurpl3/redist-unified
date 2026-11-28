//! `redist.exe`: the installer's window (see `app.rs`).
//!
//! For now the manifest is `--manifest <file>` or `manifest.json` next to the
//! exe, unsigned: development builds only. The signed manifest comes in
//! milestone 4.

#![windows_subsystem = "windows"]

use redist::context::Context;
use redist::window_manifest;

const NAME: &str = "redist";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> Result<(), slint::PlatformError> {
    redist::app::run(start())
}

/// Reads the manifest and sets up the run with Windows itself. The error is a
/// sentence for the user.
fn start() -> Result<Context, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("Could not find out where this program is: {error}."))?;
    let path = window_manifest(std::env::args().skip(1), &exe);
    let manifest = Context::read_manifest(&path)?;
    Context::prepare(manifest, NAME, VERSION)
        .map_err(|error| format!("Could not get started: {error}."))
}
