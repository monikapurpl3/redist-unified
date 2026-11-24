//! Manifest, detection and install logic for the redistributable installer.
//!
//! The client is deliberately dumb: everything component-specific (versions,
//! URLs, hashes, detection rules, silent arguments, exit codes) comes from the
//! signed manifest, which [`manifest::Manifest::parse`] reads.

pub mod detection;
pub mod log;
pub mod manifest;
pub mod session;
pub mod timestamp;
pub mod version;
#[cfg(windows)]
pub mod windows;
