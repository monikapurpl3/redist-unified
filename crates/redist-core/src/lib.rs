//! Manifest, detection and install logic for the redistributable installer.
//!
//! The client is deliberately dumb: everything component-specific (versions,
//! URLs, hashes, detection rules, silent arguments, exit codes) comes from the
//! signed manifest. See `redist-installer-plan.md`.
