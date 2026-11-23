//! The manifest: every component, where to download it, how to detect it and
//! how to install it.
//!
//! [`Manifest::parse`] checks the shape and every value, and refuses unknown
//! fields, so a typo fails loudly instead of being ignored. It does not
//! verify the signature, which is checked on the raw bytes before parsing,
//! and it does not decide whether to accept the manifest (replay protection
//! and `min_client`).

mod detect;
mod install;
mod paths;
mod values;

use std::collections::HashSet;
use std::fmt;

use serde::Deserialize;

pub use detect::{
    CommandRule, Detect, DotnetFramework, DotnetRuntimeRule, FileRule, RegistryCheck, RegistryData,
    RegistryKey, RegistryRule, RegistryView, RuleList,
};
pub use install::{ExitCodes, ExitOutcome, Installer};
pub use paths::{KnownFolder, KnownPath, RelativePath};
pub use values::{HttpsUrl, Sha256};

use crate::timestamp::Timestamp;
use crate::version::Version;

/// The only manifest schema this client understands.
pub const SCHEMA: u32 = 1;

/// A parsed and checked manifest.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Always [`SCHEMA`].
    pub schema: u32,
    /// Goes up with every published manifest. The client refuses one older
    /// than the last it saw.
    pub version: u64,
    /// When the manifest was published.
    pub issued: Timestamp,
    /// The oldest client that may use this manifest.
    pub min_client: Version,
    /// The newest client, to point the user at. The client never updates
    /// itself.
    pub latest_client: LatestClient,
    /// Every component, in the order they are installed.
    pub components: Vec<Component>,
}

impl Manifest {
    /// Parses and checks a manifest whose signature has already been
    /// verified.
    pub fn parse(json: &[u8]) -> Result<Self, ManifestError> {
        // Read the schema alone first, so a newer manifest gets "update the
        // client" rather than an error about whatever field changed.
        #[derive(Deserialize)]
        struct Probe {
            schema: u32,
        }
        let probe: Probe = serde_json::from_slice(json)?;
        if probe.schema != SCHEMA {
            return Err(ManifestError::UnsupportedSchema(probe.schema));
        }

        let manifest: Manifest = serde_json::from_slice(json)?;
        if manifest.version == 0 {
            return Err(ManifestError::Invalid("version must be at least 1".into()));
        }
        if manifest.components.is_empty() {
            return Err(ManifestError::Invalid("there are no components".into()));
        }
        let mut ids = HashSet::new();
        for component in &manifest.components {
            if !ids.insert(component.id.as_str()) {
                return Err(ManifestError::Invalid(format!(
                    "component id {:?} is used twice",
                    component.id
                )));
            }
        }
        Ok(manifest)
    }

    /// The component with this id.
    pub fn component(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }
}

/// The newest client.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatestClient {
    /// Its version.
    pub version: Version,
    /// Where the user can get it.
    pub url: HttpsUrl,
}

/// One runtime package.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawComponent")]
pub struct Component {
    /// Unique in the manifest: lowercase letters, digits and `-`.
    pub id: String,
    /// The name the user sees.
    pub name: String,
    /// The architecture it installs for.
    pub arch: Arch,
    /// The version it installs.
    pub version: Version,
    /// What to download: always, except for a Windows feature.
    pub download: Option<Download>,
    /// How to install it.
    pub installer: Installer,
    /// How to tell it is already installed.
    pub detect: Detect,
    /// What its installer's exit codes mean.
    pub exit_codes: ExitCodes,
}

/// A component as written, before its fields are checked together.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComponent {
    id: String,
    name: String,
    arch: Arch,
    version: Version,
    url: Option<HttpsUrl>,
    sha256: Option<Sha256>,
    installer: Installer,
    detect: Detect,
    exit_ok: Vec<u32>,
    exit_reboot: Vec<u32>,
    exit_already: Vec<u32>,
    exit_retry: Vec<u32>,
}

impl TryFrom<RawComponent> for Component {
    type Error = String;

    fn try_from(raw: RawComponent) -> Result<Self, String> {
        let id = raw.id;
        let fail = |problem: String| format!("component {id:?}: {problem}");

        let id_fine = (1..=64).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !id.starts_with('-')
            && !id.ends_with('-');
        if !id_fine {
            return Err(fail(
                "the id must be 1 to 64 lowercase letters, digits and -, not starting or ending with -"
                    .into(),
            ));
        }
        if raw.name.trim().is_empty() || raw.name.chars().any(char::is_control) {
            return Err(fail("the name is empty or has control characters".into()));
        }

        let download = match (raw.installer.downloads(), raw.url, raw.sha256) {
            (true, Some(url), Some(sha256)) => Some(Download { url, sha256 }),
            (true, _, _) => return Err(fail("its installer needs url and sha256".into())),
            (false, None, None) => None,
            (false, _, _) => {
                return Err(fail(
                    "a windows_feature installer downloads nothing; drop url and sha256".into(),
                ));
            }
        };
        raw.installer.check().map_err(fail)?;
        let exit_codes = ExitCodes::new(
            raw.exit_ok,
            raw.exit_reboot,
            raw.exit_already,
            raw.exit_retry,
        )
        .map_err(fail)?;

        Ok(Component {
            id,
            name: raw.name,
            arch: raw.arch,
            version: raw.version,
            download,
            installer: raw.installer,
            detect: raw.detect,
            exit_codes,
        })
    }
}

/// Where to download a component, and the hash it must have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    /// The installer's URL.
    pub url: HttpsUrl,
    /// Its SHA-256, checked before it runs.
    pub sha256: Sha256,
}

/// The architecture a component installs for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    /// 32-bit x86.
    X86,
    /// x64.
    X64,
    /// ARM64.
    Arm64,
    /// Not tied to one architecture, such as a Windows feature.
    Any,
}

/// Why a manifest was refused.
#[derive(Debug)]
pub enum ManifestError {
    /// Not JSON, or a field is missing, unknown, of the wrong type or has a
    /// bad value.
    Json(serde_json::Error),
    /// A schema this client does not understand; a newer client is needed.
    UnsupportedSchema(u32),
    /// Fields that are fine one by one but not together.
    Invalid(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Json(error) => write!(f, "the manifest is not valid: {error}"),
            ManifestError::UnsupportedSchema(schema) => write!(
                f,
                "the manifest uses schema {schema}, but this client only understands schema {SCHEMA}"
            ),
            ManifestError::Invalid(problem) => write!(f, "the manifest is not valid: {problem}"),
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ManifestError::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for ManifestError {
    fn from(error: serde_json::Error) -> Self {
        ManifestError::Json(error)
    }
}
