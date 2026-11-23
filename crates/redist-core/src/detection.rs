//! Deciding whether a component is already installed.
//!
//! The rules come from the manifest ([`Detect`]). The facts come from a
//! [`Machine`]: Windows itself in the client
//! ([`WindowsMachine`](crate::windows::WindowsMachine)), a fake in the tests.
//!
//! When in doubt, detection leans towards "not installed". A value of the
//! wrong type, an unparsable version or a preview .NET runtime just fails its
//! rule. Reinstalling a present runtime is harmless (its installer reports
//! "already installed"), while skipping a missing one would leave the user's
//! program broken.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::manifest::{
    Arch, CommandRule, Component, Detect, DotnetRuntimeRule, FileRule, KnownFolder, KnownPath,
    RegistryCheck, RegistryData, RegistryRule, RegistryView,
};
use crate::version::Version;

/// The architecture of the Windows the client runs on. The client is a 64-bit
/// program, so it never runs on 32-bit Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeArch {
    /// x64 Windows.
    X64,
    /// ARM64 Windows.
    Arm64,
}

/// A registry value, as far as detection is concerned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryValue {
    /// `REG_DWORD`.
    Dword(u32),
    /// `REG_QWORD`.
    Qword(u64),
    /// `REG_SZ` or `REG_EXPAND_SZ`, not expanded.
    String(String),
    /// Any other type. No rule matches it.
    Other,
}

/// What detection asks the machine.
///
/// Something that is not there is an answer (`Ok(false)`, `Ok(None)`, an
/// empty list). `Err` means the question could not be answered, such as
/// access denied.
pub trait Machine {
    /// The architecture of Windows itself.
    fn native_arch(&self) -> NativeArch;

    /// Where a known folder is.
    fn known_folder(&self, folder: KnownFolder) -> io::Result<PathBuf>;

    /// Whether `HKLM\<subkey>` exists in `view`.
    fn registry_key_exists(&self, view: RegistryView, subkey: &str) -> io::Result<bool>;

    /// A value of `HKLM\<subkey>` in `view`, or `None` if the key or value is
    /// missing. `""` names the key's default value.
    fn registry_value(
        &self,
        view: RegistryView,
        subkey: &str,
        name: &str,
    ) -> io::Result<Option<RegistryValue>>;

    /// Whether a file (not a folder) exists.
    fn file_exists(&self, path: &Path) -> io::Result<bool>;

    /// A file's version resource, or `None` if the file is missing or has
    /// none.
    fn file_version(&self, path: &Path) -> io::Result<Option<Version>>;

    /// The names of the folders in a folder, or none if it is missing.
    fn subfolders(&self, path: &Path) -> io::Result<Vec<String>>;

    /// Runs a program that exists, without a window, and returns its exit
    /// code.
    fn run(&self, program: &Path, args: &[String]) -> io::Result<u32>;
}

/// Whether a component for `arch` belongs on a machine.
///
/// x86 and `any` components belong everywhere. x64 ones belong on ARM64
/// too, because x64 programs run there under emulation and need their own
/// runtimes. ARM64 ones belong on ARM64 only.
pub fn applies_to(arch: Arch, native: NativeArch) -> bool {
    match arch {
        Arch::Any | Arch::X86 | Arch::X64 => true,
        Arch::Arm64 => native == NativeArch::Arm64,
    }
}

/// Whether `component` is installed on `machine`.
///
/// Fails with [`DetectError::NotForThisMachine`] for a component that does
/// not [apply](applies_to) to the machine, and with [`DetectError::Probe`]
/// when the machine cannot answer a question that decides the result.
pub fn is_installed(component: &Component, machine: &dyn Machine) -> Result<bool, DetectError> {
    let native = machine.native_arch();
    if !applies_to(component.arch, native) {
        return Err(DetectError::NotForThisMachine {
            arch: component.arch,
            native,
        });
    }
    Checker {
        machine,
        arch: component.arch,
        native,
    }
    .check(&component.detect)
}

/// Why detection could not decide.
#[derive(Debug)]
pub enum DetectError {
    /// The component is for an architecture this machine does not use.
    NotForThisMachine { arch: Arch, native: NativeArch },
    /// The machine could not answer a question about `what`.
    Probe { what: String, source: io::Error },
}

impl fmt::Display for DetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DetectError::NotForThisMachine { arch, native } => write!(
                f,
                "a component for {arch:?} does not belong on {native:?} Windows"
            ),
            DetectError::Probe { what, source } => write!(f, "could not check {what}: {source}"),
        }
    }
}

impl std::error::Error for DetectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DetectError::Probe { source, .. } => Some(source),
            DetectError::NotForThisMachine { .. } => None,
        }
    }
}

/// Evaluates the rules of one component.
struct Checker<'a> {
    machine: &'a dyn Machine,
    arch: Arch,
    native: NativeArch,
}

impl Checker<'_> {
    fn check(&self, rule: &Detect) -> Result<bool, DetectError> {
        match rule {
            Detect::Registry(rule) => self.registry(rule),
            Detect::File(rule) => self.file(rule),
            Detect::DotnetRuntime(rule) => self.dotnet(rule),
            Detect::Command(rule) => self.command(rule),
            Detect::All { rules } => self.combine(rules.as_slice(), false),
            Detect::Any { rules } => self.combine(rules.as_slice(), true),
        }
    }

    /// `all` (decisive `false`) and `any` (decisive `true`). The first rule
    /// with the decisive result settles it, even if another rule failed to
    /// check. Without one, a failure is the answer, and otherwise the
    /// opposite of the decisive result.
    fn combine(&self, rules: &[Detect], decisive: bool) -> Result<bool, DetectError> {
        let mut failure = None;
        for rule in rules {
            match self.check(rule) {
                Ok(result) if result == decisive => return Ok(decisive),
                Ok(_) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(!decisive),
        }
    }

    fn registry(&self, rule: &RegistryRule) -> Result<bool, DetectError> {
        let subkey = rule.key.subkey();
        let what = || {
            let view = match rule.view {
                RegistryView::Bits32 => 32,
                RegistryView::Bits64 => 64,
            };
            match &rule.value {
                Some(name) => format!("HKLM\\{subkey} value {name:?} ({view}-bit view)"),
                None => format!("HKLM\\{subkey} ({view}-bit view)"),
            }
        };
        // A rule without a value name only ever checks that the key exists.
        let Some(name) = &rule.value else {
            return self
                .machine
                .registry_key_exists(rule.view, subkey)
                .map_err(|source| probe(what(), source));
        };
        let Some(value) = self
            .machine
            .registry_value(rule.view, subkey, name)
            .map_err(|source| probe(what(), source))?
        else {
            return Ok(false);
        };
        Ok(match (&rule.check, &value) {
            (RegistryCheck::Exists, _) => true,
            (RegistryCheck::Min(min), RegistryValue::Dword(n)) => u64::from(*n) >= *min,
            (RegistryCheck::Min(min), RegistryValue::Qword(n)) => n >= min,
            (RegistryCheck::Equals(RegistryData::Number(want)), RegistryValue::Dword(n)) => {
                u64::from(*n) == *want
            }
            (RegistryCheck::Equals(RegistryData::Number(want)), RegistryValue::Qword(n)) => {
                n == want
            }
            (RegistryCheck::Equals(RegistryData::String(want)), RegistryValue::String(s)) => {
                s == want
            }
            (RegistryCheck::MinVersion(min), RegistryValue::String(s)) => {
                s.trim().parse::<Version>().is_ok_and(|v| v >= *min)
            }
            _ => false,
        })
    }

    fn file(&self, rule: &FileRule) -> Result<bool, DetectError> {
        let path = self.resolve(&rule.path)?;
        let what = || path.display().to_string();
        match &rule.min_version {
            None => self
                .machine
                .file_exists(&path)
                .map_err(|source| probe(what(), source)),
            Some(min) => Ok(self
                .machine
                .file_version(&path)
                .map_err(|source| probe(what(), source))?
                .is_some_and(|version| version >= *min)),
        }
    }

    fn dotnet(&self, rule: &DotnetRuntimeRule) -> Result<bool, DetectError> {
        // Where the dotnet of the component's architecture lives. x64 .NET on
        // ARM64 Windows has its own folder below the native one.
        let root = match (self.native, self.arch) {
            (_, Arch::X86) => join(&self.folder(KnownFolder::ProgramFilesX86)?, "dotnet"),
            (NativeArch::X64, Arch::X64) | (NativeArch::Arm64, Arch::Arm64) => {
                join(&self.folder(KnownFolder::ProgramFiles)?, "dotnet")
            }
            (NativeArch::Arm64, Arch::X64) => {
                join(&self.folder(KnownFolder::ProgramFiles)?, r"dotnet\x64")
            }
            // `applies_to` and the manifest (no dotnet rule in an `any`
            // component) rule these out.
            (native, arch) => return Err(DetectError::NotForThisMachine { arch, native }),
        };
        let name = rule.framework.folder_name();
        let frameworks = join(&root, &format!(r"shared\{name}"));
        let what = || format!(".NET {name} {} in {}", rule.min, frameworks.display());

        let folders = self
            .machine
            .subfolders(&frameworks)
            .map_err(|source| probe(what(), source))?;
        for folder in folders {
            // Previews such as 9.0.0-rc.2 do not parse, and never count.
            let Ok(version) = folder.parse::<Version>() else {
                continue;
            };
            if version.major() != rule.min.major() || version < rule.min {
                continue;
            }
            // An uninstall can leave an empty version folder behind; a real
            // runtime has its deps.json.
            let marker = join(&frameworks, &format!(r"{folder}\{name}.deps.json"));
            if self
                .machine
                .file_exists(&marker)
                .map_err(|source| probe(what(), source))?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn command(&self, rule: &CommandRule) -> Result<bool, DetectError> {
        let program = self.resolve(&rule.program)?;
        let what = || format!("the exit code of {}", program.display());
        if !self
            .machine
            .file_exists(&program)
            .map_err(|source| probe(what(), source))?
        {
            return Ok(false);
        }
        let code = self
            .machine
            .run(&program, &rule.args)
            .map_err(|source| probe(what(), source))?;
        Ok(rule.exit_installed.contains(&code))
    }

    fn folder(&self, folder: KnownFolder) -> Result<PathBuf, DetectError> {
        self.machine
            .known_folder(folder)
            .map_err(|source| probe(format!("where {{{}}} is", folder.name()), source))
    }

    fn resolve(&self, path: &KnownPath) -> Result<PathBuf, DetectError> {
        Ok(join(&self.folder(path.folder())?, path.relative().as_str()))
    }
}

fn probe(what: String, source: io::Error) -> DetectError {
    DetectError::Probe { what, source }
}

/// Joins with a backslash on every platform, so the tests see the same paths
/// as Windows.
fn join(base: &Path, relative: &str) -> PathBuf {
    let base = base.to_string_lossy();
    PathBuf::from(format!("{}\\{relative}", base.trim_end_matches('\\')))
}
