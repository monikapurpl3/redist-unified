//! Detection rules: how the client tells that a component is already
//! installed.

use serde::Deserialize;

use super::paths::KnownPath;
use crate::version::Version;

/// A detection rule. A component counts as installed when its rule holds.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Detect {
    /// A registry key or value, optionally compared.
    Registry(RegistryRule),
    /// A file, optionally with a minimum file version.
    File(FileRule),
    /// A shared .NET runtime of the component's architecture.
    DotnetRuntime(DotnetRuntimeRule),
    /// A program's exit code. A last resort, for what nothing else detects.
    Command(CommandRule),
    /// Every rule holds.
    All { rules: RuleList },
    /// At least one rule holds.
    Any { rules: RuleList },
}

impl Detect {
    /// Whether this rule or any rule inside it is a `dotnet_runtime` rule.
    pub(super) fn uses_dotnet(&self) -> bool {
        match self {
            Detect::DotnetRuntime(_) => true,
            Detect::All { rules } | Detect::Any { rules } => {
                rules.as_slice().iter().any(Detect::uses_dotnet)
            }
            _ => false,
        }
    }
}

/// The rules of `all` or `any`. Never empty.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<Detect>")]
pub struct RuleList(Vec<Detect>);

impl RuleList {
    /// The rules, in manifest order.
    pub fn as_slice(&self) -> &[Detect] {
        &self.0
    }
}

impl TryFrom<Vec<Detect>> for RuleList {
    type Error = String;

    fn try_from(rules: Vec<Detect>) -> Result<Self, String> {
        if rules.is_empty() {
            Err("`all` and `any` need at least one rule".into())
        } else {
            Ok(RuleList(rules))
        }
    }
}

/// A registry check under `HKLM`, written in the manifest as `key`, `view`,
/// an optional `value` name, and at most one of `min`, `equals` and
/// `min_version`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawRegistryRule")]
pub struct RegistryRule {
    /// The key.
    pub key: RegistryKey,
    /// Which registry view to read.
    pub view: RegistryView,
    /// The value name, or `None` to check the key alone. `""` is the key's
    /// default value.
    pub value: Option<String>,
    /// What must be true of the key or value.
    pub check: RegistryCheck,
}

/// A registry rule as written, before its fields are checked together.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistryRule {
    key: RegistryKey,
    view: RegistryView,
    value: Option<String>,
    min: Option<u64>,
    equals: Option<RegistryData>,
    min_version: Option<Version>,
}

impl TryFrom<RawRegistryRule> for RegistryRule {
    type Error = String;

    fn try_from(raw: RawRegistryRule) -> Result<Self, String> {
        let check = match (raw.min, raw.equals, raw.min_version) {
            (None, None, None) => RegistryCheck::Exists,
            (Some(min), None, None) => RegistryCheck::Min(min),
            (None, Some(data), None) => RegistryCheck::Equals(data),
            (None, None, Some(version)) => RegistryCheck::MinVersion(version),
            _ => {
                return Err(
                    "a registry rule takes at most one of min, equals and min_version".into(),
                );
            }
        };
        if raw
            .value
            .as_deref()
            .is_some_and(|name| name.chars().any(char::is_control))
        {
            return Err("a registry value name cannot have control characters".into());
        }
        if check != RegistryCheck::Exists && raw.value.is_none() {
            return Err(
                "a registry rule with min, equals or min_version needs a value name".into(),
            );
        }
        Ok(RegistryRule {
            key: raw.key,
            view: raw.view,
            value: raw.value,
            check,
        })
    }
}

/// What a registry rule requires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryCheck {
    /// The value exists, or the key when no value is named.
    Exists,
    /// A DWORD or QWORD value is at least this.
    Min(u64),
    /// The value equals this number or string.
    Equals(RegistryData),
    /// A string value holds a dotted version at least this new.
    MinVersion(Version),
}

/// A number or a string to compare a registry value with.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum RegistryData {
    /// Compared with a DWORD or QWORD value.
    Number(u64),
    /// Compared with a string value.
    String(String),
}

/// A key below `HKEY_LOCAL_MACHINE`, written `HKLM\...` in the manifest.
///
/// Only machine-wide keys are checked. `WOW6432Node` is refused: name the key
/// as a 64-bit program sees it and set the [`RegistryView`] instead, which
/// also works on 32-bit Windows.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct RegistryKey {
    subkey: String,
}

impl RegistryKey {
    /// The path below `HKEY_LOCAL_MACHINE`.
    pub fn subkey(&self) -> &str {
        &self.subkey
    }
}

impl TryFrom<String> for RegistryKey {
    type Error = String;

    fn try_from(key: String) -> Result<Self, String> {
        let Some(subkey) = key
            .strip_prefix("HKLM\\")
            .or_else(|| key.strip_prefix("HKEY_LOCAL_MACHINE\\"))
        else {
            return Err(format!(
                "registry key {key:?} is not under HKLM; only machine-wide keys are checked"
            ));
        };
        if subkey
            .split('\\')
            .any(|part| part.is_empty() || part.chars().any(char::is_control))
        {
            return Err(format!(
                "registry key {key:?} has an empty part or a control character"
            ));
        }
        if subkey
            .split('\\')
            .any(|part| part.eq_ignore_ascii_case("WOW6432Node"))
        {
            return Err(format!(
                "registry key {key:?} names WOW6432Node; drop it and set \"view\": \"32\""
            ));
        }
        Ok(RegistryKey {
            subkey: subkey.to_owned(),
        })
    }
}

/// Which registry view a rule reads, whatever the client's own bitness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum RegistryView {
    /// `"32"`: what 32-bit programs see (`WOW6432Node` on 64-bit Windows).
    #[serde(rename = "32")]
    Bits32,
    /// `"64"`: what 64-bit programs see.
    #[serde(rename = "64")]
    Bits64,
}

/// A file that must exist.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRule {
    /// The file, such as `{system32}\d3dx9_43.dll`.
    pub path: KnownPath,
    /// If given, the file's version resource must be at least this.
    pub min_version: Option<Version>,
}

/// A shared .NET runtime of the component's architecture.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DotnetRuntimeRule {
    /// Which shared framework.
    pub framework: DotnetFramework,
    /// Installed when a version with the same major, equal to or newer than
    /// this, is present.
    pub min: Version,
}

/// The shared .NET frameworks, named as their folders under
/// `dotnet\shared`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum DotnetFramework {
    /// The plain runtime, for console programs.
    #[serde(rename = "Microsoft.NETCore.App")]
    NetCore,
    /// The Desktop Runtime (Windows Forms and WPF).
    #[serde(rename = "Microsoft.WindowsDesktop.App")]
    WindowsDesktop,
    /// The ASP.NET Core runtime.
    #[serde(rename = "Microsoft.AspNetCore.App")]
    AspNetCore,
}

impl DotnetFramework {
    /// The framework's folder name under `dotnet\shared`.
    pub fn folder_name(self) -> &'static str {
        match self {
            DotnetFramework::NetCore => "Microsoft.NETCore.App",
            DotnetFramework::WindowsDesktop => "Microsoft.WindowsDesktop.App",
            DotnetFramework::AspNetCore => "Microsoft.AspNetCore.App",
        }
    }
}

/// Run a program and look at its exit code. Nothing else about it (output,
/// language) is used.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawCommandRule")]
pub struct CommandRule {
    /// The program, from a known folder, so nothing is found through `PATH`.
    pub program: KnownPath,
    /// Its arguments.
    pub args: Vec<String>,
    /// The exit codes that mean "installed". Never empty.
    pub exit_installed: Vec<u32>,
}

/// A command rule as written, before its fields are checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommandRule {
    program: KnownPath,
    args: Vec<String>,
    exit_installed: Vec<u32>,
}

impl TryFrom<RawCommandRule> for CommandRule {
    type Error = String;

    fn try_from(raw: RawCommandRule) -> Result<Self, String> {
        if raw.exit_installed.is_empty() {
            return Err("a command rule needs at least one code in exit_installed".into());
        }
        Ok(CommandRule {
            program: raw.program,
            args: raw.args,
            exit_installed: raw.exit_installed,
        })
    }
}
