//! The fixture, a fake machine, and helpers shared by the integration tests.
//!
//! The fixture's URLs and hashes are dummies. Its silent arguments and
//! detection values only exercise the code: none of them has been checked
//! against the real installers, so do not copy them into a real manifest.

// Each test file uses its own subset.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use redist_core::detection::{Machine, NativeArch, RegistryValue};
use redist_core::manifest::{KnownFolder, Manifest, ManifestError, RegistryView};
use redist_core::version::Version;
use serde_json::Value;

pub const FIXTURE: &str = include_str!("../data/manifest.json");

// Component positions in the fixture.
pub const VC_X64: usize = 0;
pub const VC_X86: usize = 1;
pub const DIRECTX: usize = 2;
pub const DOTNET: usize = 3;
pub const NETFX: usize = 4;

pub const VC_KEY_X64: &str = r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64";
pub const VC_KEY_X86: &str = r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x86";
pub const NETFX_KEY: &str = r"SOFTWARE\Microsoft\NET Framework Setup\NDP\v3.5";
pub const DESKTOP: &str = "Microsoft.WindowsDesktop.App";

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

/// A machine made of maps. Like Windows, it ignores the case of registry
/// keys, value names and paths. It can change while a test runs, so a fake
/// installer can "install" something.
pub struct FakeMachine {
    native: NativeArch,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// Registry keys by view and lowercase path, each with its values by
    /// lowercase name.
    keys: HashMap<(RegistryView, String), HashMap<String, RegistryValue>>,
    /// Files by lowercase path, with their version resource.
    files: HashMap<String, Option<Version>>,
    /// Folders by lowercase path, with the names of their subfolders.
    folders: HashMap<String, Vec<String>>,
    /// Exit codes of the programs a command rule may run.
    exit_codes: HashMap<String, u32>,
    /// Key or path prefixes (lowercase) that answer "access denied".
    denied: Vec<String>,
    /// Every program a command rule ran, with its arguments.
    runs: Vec<(String, Vec<String>)>,
}

pub fn x64() -> FakeMachine {
    FakeMachine::new(NativeArch::X64)
}

pub fn arm64() -> FakeMachine {
    FakeMachine::new(NativeArch::Arm64)
}

/// Adds both VC++ 2015-2022 runtimes at build `bld`.
pub fn with_vc(machine: FakeMachine, bld: u32) -> FakeMachine {
    use RegistryView::{Bits32, Bits64};
    machine
        .value(Bits64, VC_KEY_X64, "Installed", RegistryValue::Dword(1))
        .value(Bits64, VC_KEY_X64, "Bld", RegistryValue::Dword(bld))
        .value(Bits32, VC_KEY_X86, "Installed", RegistryValue::Dword(1))
        .value(Bits32, VC_KEY_X86, "Bld", RegistryValue::Dword(bld))
}

impl FakeMachine {
    pub fn new(native: NativeArch) -> Self {
        FakeMachine {
            native,
            state: RefCell::new(State::default()),
        }
    }

    pub fn key(self, view: RegistryView, subkey: &str) -> Self {
        self.add_key(view, subkey);
        self
    }

    pub fn value(self, view: RegistryView, subkey: &str, name: &str, value: RegistryValue) -> Self {
        self.set_value(view, subkey, name, value);
        self
    }

    /// Adds a file, and every folder on its path.
    pub fn file(self, path: &str, version: Option<&str>) -> Self {
        self.add_file(path, version);
        self
    }

    /// Adds an empty folder, and every folder on its path.
    pub fn folder(self, path: &str) -> Self {
        self.state.borrow_mut().add_folder(path);
        self
    }

    /// Adds a .NET runtime as its installer leaves it: the version folder
    /// with the framework's deps.json in it.
    pub fn dotnet(self, root: &str, framework: &str, version: &str) -> Self {
        self.add_dotnet(root, framework, version);
        self
    }

    /// Adds a program for a command rule, with the exit code it returns.
    pub fn program(self, path: &str, exit_code: u32) -> Self {
        self.add_file(path, None);
        self.state
            .borrow_mut()
            .exit_codes
            .insert(path.to_lowercase(), exit_code);
        self
    }

    /// Makes every key or path that starts with `prefix` answer "access
    /// denied".
    pub fn deny(self, prefix: &str) -> Self {
        self.state.borrow_mut().denied.push(prefix.to_lowercase());
        self
    }

    pub fn add_key(&self, view: RegistryView, subkey: &str) {
        self.state
            .borrow_mut()
            .keys
            .entry((view, subkey.to_lowercase()))
            .or_default();
    }

    pub fn set_value(&self, view: RegistryView, subkey: &str, name: &str, value: RegistryValue) {
        self.state
            .borrow_mut()
            .keys
            .entry((view, subkey.to_lowercase()))
            .or_default()
            .insert(name.to_lowercase(), value);
    }

    pub fn add_file(&self, path: &str, version: Option<&str>) {
        let mut state = self.state.borrow_mut();
        state
            .files
            .insert(path.to_lowercase(), version.map(|v| v.parse().unwrap()));
        state.add_folder(path.rsplit_once('\\').unwrap().0);
    }

    pub fn add_dotnet(&self, root: &str, framework: &str, version: &str) {
        self.add_file(
            &format!(r"{root}\shared\{framework}\{version}\{framework}.deps.json"),
            None,
        );
    }

    /// Every program a command rule ran, with its arguments.
    pub fn runs(&self) -> Vec<(String, Vec<String>)> {
        self.state.borrow().runs.clone()
    }

    fn check_access(&self, what: &str) -> io::Result<String> {
        let what = what.to_lowercase();
        if self
            .state
            .borrow()
            .denied
            .iter()
            .any(|prefix| what.starts_with(prefix))
        {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "access denied",
            ))
        } else {
            Ok(what)
        }
    }
}

impl State {
    fn add_folder(&mut self, path: &str) {
        self.folders.entry(path.to_lowercase()).or_default();
        if let Some((parent, name)) = path.rsplit_once('\\') {
            let names = self.folders.entry(parent.to_lowercase()).or_default();
            if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                names.push(name.to_owned());
            }
            self.add_folder(parent);
        }
    }
}

impl Machine for FakeMachine {
    fn native_arch(&self) -> NativeArch {
        self.native
    }

    fn known_folder(&self, folder: KnownFolder) -> io::Result<PathBuf> {
        Ok(PathBuf::from(match folder {
            KnownFolder::Windows => r"C:\Windows",
            KnownFolder::System32 => r"C:\Windows\System32",
            KnownFolder::SysWow64 => r"C:\Windows\SysWOW64",
            KnownFolder::ProgramFiles => r"C:\Program Files",
            KnownFolder::ProgramFilesX86 => r"C:\Program Files (x86)",
        }))
    }

    fn registry_key_exists(&self, view: RegistryView, subkey: &str) -> io::Result<bool> {
        let subkey = self.check_access(subkey)?;
        Ok(self.state.borrow().keys.contains_key(&(view, subkey)))
    }

    fn registry_value(
        &self,
        view: RegistryView,
        subkey: &str,
        name: &str,
    ) -> io::Result<Option<RegistryValue>> {
        let subkey = self.check_access(subkey)?;
        Ok(self
            .state
            .borrow()
            .keys
            .get(&(view, subkey))
            .and_then(|values| values.get(&name.to_lowercase()))
            .cloned())
    }

    fn file_exists(&self, path: &Path) -> io::Result<bool> {
        let path = self.check_access(&path.to_string_lossy())?;
        Ok(self.state.borrow().files.contains_key(&path))
    }

    fn file_version(&self, path: &Path) -> io::Result<Option<Version>> {
        let path = self.check_access(&path.to_string_lossy())?;
        Ok(self.state.borrow().files.get(&path).copied().flatten())
    }

    fn subfolders(&self, path: &Path) -> io::Result<Vec<String>> {
        let path = self.check_access(&path.to_string_lossy())?;
        Ok(self
            .state
            .borrow()
            .folders
            .get(&path)
            .cloned()
            .unwrap_or_default())
    }

    fn run(&self, program: &Path, args: &[String]) -> io::Result<u32> {
        let program = program.to_string_lossy().into_owned();
        let mut state = self.state.borrow_mut();
        state.runs.push((program.clone(), args.to_vec()));
        state
            .exit_codes
            .get(&program.to_lowercase())
            .copied()
            .ok_or_else(|| io::Error::other("the fake has no exit code for this program"))
    }
}
