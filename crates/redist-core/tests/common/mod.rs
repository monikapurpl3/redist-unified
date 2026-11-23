//! The fixture and helpers shared by the integration tests.
//!
//! The fixture's URLs and hashes are dummies. Its silent arguments and
//! detection values only exercise the code: none of them has been checked
//! against the real installers, so do not copy them into a real manifest.

// Each test file uses its own subset.
#![allow(dead_code)]

use redist_core::manifest::{Manifest, ManifestError};
use serde_json::Value;

pub const FIXTURE: &str = include_str!("../data/manifest.json");

// Component positions in the fixture.
pub const VC_X64: usize = 0;
pub const VC_X86: usize = 1;
pub const DIRECTX: usize = 2;
pub const DOTNET: usize = 3;
pub const NETFX: usize = 4;

pub fn fixture() -> Manifest {
    Manifest::parse(FIXTURE.as_bytes()).unwrap()
}

/// Parses the fixture after `edit` has changed it.
pub fn parse_edited(edit: impl FnOnce(&mut Value)) -> Result<Manifest, ManifestError> {
    let mut manifest: Value = serde_json::from_str(FIXTURE).unwrap();
    edit(&mut manifest);
    Manifest::parse(&serde_json::to_vec(&manifest).unwrap())
}

pub fn component(manifest: &mut Value, index: usize) -> &mut Value {
    &mut manifest["components"][index]
}

pub fn remove(object: &mut Value, field: &str) {
    object.as_object_mut().unwrap().remove(field);
}
