//! Detection against a fake machine.

mod common;

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use common::{DIRECTX, DOTNET, NETFX, VC_X64, VC_X86, component, fixture, parse_edited};
use redist_core::detection::{
    DetectError, Machine, NativeArch, RegistryValue, applies_to, is_installed,
};
use redist_core::manifest::{Arch, Component, KnownFolder, Manifest, RegistryView};
use redist_core::version::Version;
use serde_json::json;

const VC_KEY_X64: &str = r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64";
const VC_KEY_X86: &str = r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x86";
const NETFX_KEY: &str = r"SOFTWARE\Microsoft\NET Framework Setup\NDP\v3.5";
const DESKTOP: &str = "Microsoft.WindowsDesktop.App";

/// A machine made of maps. Like Windows, it ignores the case of registry
/// keys, value names and paths.
struct FakeMachine {
    native: NativeArch,
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
    /// Every program run, with its arguments.
    runs: RefCell<Vec<(String, Vec<String>)>>,
}

impl FakeMachine {
    fn new(native: NativeArch) -> Self {
        FakeMachine {
            native,
            keys: HashMap::new(),
            files: HashMap::new(),
            folders: HashMap::new(),
            exit_codes: HashMap::new(),
            denied: Vec::new(),
            runs: RefCell::new(Vec::new()),
        }
    }

    fn key(mut self, view: RegistryView, subkey: &str) -> Self {
        self.keys.entry((view, subkey.to_lowercase())).or_default();
        self
    }

    fn value(mut self, view: RegistryView, subkey: &str, name: &str, value: RegistryValue) -> Self {
        self.keys
            .entry((view, subkey.to_lowercase()))
            .or_default()
            .insert(name.to_lowercase(), value);
        self
    }

    /// Adds a file, and every folder on its path.
    fn file(mut self, path: &str, version: Option<&str>) -> Self {
        self.files
            .insert(path.to_lowercase(), version.map(|v| v.parse().unwrap()));
        let parent = path.rsplit_once('\\').unwrap().0;
        self.add_folder(parent);
        self
    }

    /// Adds an empty folder, and every folder on its path.
    fn folder(mut self, path: &str) -> Self {
        self.add_folder(path);
        self
    }

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

    /// Adds a .NET runtime as its installer leaves it: the version folder
    /// with the framework's deps.json in it.
    fn dotnet(self, root: &str, framework: &str, version: &str) -> Self {
        self.file(
            &format!(r"{root}\shared\{framework}\{version}\{framework}.deps.json"),
            None,
        )
    }

    fn program(mut self, path: &str, exit_code: u32) -> Self {
        self = self.file(path, None);
        self.exit_codes.insert(path.to_lowercase(), exit_code);
        self
    }

    fn deny(mut self, prefix: &str) -> Self {
        self.denied.push(prefix.to_lowercase());
        self
    }

    fn check_access(&self, what: &str) -> io::Result<String> {
        let what = what.to_lowercase();
        if self.denied.iter().any(|prefix| what.starts_with(prefix)) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "access denied",
            ))
        } else {
            Ok(what)
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
        Ok(self.keys.contains_key(&(view, subkey)))
    }

    fn registry_value(
        &self,
        view: RegistryView,
        subkey: &str,
        name: &str,
    ) -> io::Result<Option<RegistryValue>> {
        let subkey = self.check_access(subkey)?;
        Ok(self
            .keys
            .get(&(view, subkey))
            .and_then(|values| values.get(&name.to_lowercase()))
            .cloned())
    }

    fn file_exists(&self, path: &Path) -> io::Result<bool> {
        let path = self.check_access(&path.to_string_lossy())?;
        Ok(self.files.contains_key(&path))
    }

    fn file_version(&self, path: &Path) -> io::Result<Option<Version>> {
        let path = self.check_access(&path.to_string_lossy())?;
        Ok(self.files.get(&path).copied().flatten())
    }

    fn subfolders(&self, path: &Path) -> io::Result<Vec<String>> {
        let path = self.check_access(&path.to_string_lossy())?;
        Ok(self.folders.get(&path).cloned().unwrap_or_default())
    }

    fn run(&self, program: &Path, args: &[String]) -> io::Result<u32> {
        let program = program.to_string_lossy().into_owned();
        self.runs
            .borrow_mut()
            .push((program.clone(), args.to_vec()));
        self.exit_codes
            .get(&program.to_lowercase())
            .copied()
            .ok_or_else(|| io::Error::other("the fake has no exit code for this program"))
    }
}

fn x64() -> FakeMachine {
    FakeMachine::new(NativeArch::X64)
}

fn arm64() -> FakeMachine {
    FakeMachine::new(NativeArch::Arm64)
}

fn installed(component: &Component, machine: &FakeMachine) -> bool {
    is_installed(component, machine).unwrap()
}

/// A machine with both VC++ 2015-2022 runtimes at build `bld`.
fn with_vc(machine: FakeMachine, bld: u32) -> FakeMachine {
    use RegistryView::{Bits32, Bits64};
    machine
        .value(Bits64, VC_KEY_X64, "Installed", RegistryValue::Dword(1))
        .value(Bits64, VC_KEY_X64, "Bld", RegistryValue::Dword(bld))
        .value(Bits32, VC_KEY_X86, "Installed", RegistryValue::Dword(1))
        .value(Bits32, VC_KEY_X86, "Bld", RegistryValue::Dword(bld))
}

#[test]
fn a_clean_machine_has_nothing() {
    let manifest = fixture();
    for native in [x64(), arm64()] {
        for component in &manifest.components {
            assert!(!installed(component, &native), "{} found", component.id);
        }
    }
}

#[test]
fn vc_needs_installed_and_a_new_enough_build() {
    let manifest = fixture();
    let (vc64, vc86) = (&manifest.components[VC_X64], &manifest.components[VC_X86]);

    let machine = with_vc(x64(), 35211);
    assert!(installed(vc64, &machine));
    assert!(installed(vc86, &machine));

    let machine = with_vc(x64(), 40000);
    assert!(installed(vc64, &machine));

    let machine = with_vc(x64(), 35208);
    assert!(!installed(vc64, &machine));
    assert!(!installed(vc86, &machine));

    // Installed = 0 is what an uninstall can leave behind.
    let machine = with_vc(x64(), 35211).value(
        RegistryView::Bits64,
        VC_KEY_X64,
        "Installed",
        RegistryValue::Dword(0),
    );
    assert!(!installed(vc64, &machine));
}

#[test]
fn registry_views_are_kept_apart() {
    let manifest = fixture();
    // The x86 runtime's key in the 64-bit view is not the x86 runtime.
    let machine = x64()
        .value(
            RegistryView::Bits64,
            VC_KEY_X86,
            "Installed",
            RegistryValue::Dword(1),
        )
        .value(
            RegistryView::Bits64,
            VC_KEY_X86,
            "Bld",
            RegistryValue::Dword(35211),
        );
    assert!(!installed(&manifest.components[VC_X86], &machine));
}

#[test]
fn registry_names_ignore_case() {
    let manifest = fixture();
    let machine = x64()
        .value(
            RegistryView::Bits64,
            &VC_KEY_X64.to_uppercase(),
            "INSTALLED",
            RegistryValue::Dword(1),
        )
        .value(
            RegistryView::Bits64,
            VC_KEY_X64,
            "bld",
            RegistryValue::Qword(35211),
        );
    assert!(installed(&manifest.components[VC_X64], &machine));
}

#[test]
fn a_value_of_the_wrong_type_fails_its_rule() {
    let manifest = fixture();
    let machine = x64()
        .value(
            RegistryView::Bits64,
            VC_KEY_X64,
            "Installed",
            RegistryValue::Dword(1),
        )
        .value(
            RegistryView::Bits64,
            VC_KEY_X64,
            "Bld",
            RegistryValue::String("35211".into()),
        );
    assert!(!installed(&manifest.components[VC_X64], &machine));

    let machine = x64().value(
        RegistryView::Bits64,
        NETFX_KEY,
        "Install",
        RegistryValue::Other,
    );
    assert!(!installed(&manifest.components[NETFX], &machine));
}

#[test]
fn registry_presence_strings_and_versions() {
    // Value present, any type.
    let manifest = parse_edited(|m| {
        component(m, NETFX)["detect"] = json!({
            "type": "registry", "key": format!("HKLM\\{NETFX_KEY}"), "view": "64", "value": "Version"
        });
    })
    .unwrap();
    let netfx = &manifest.components[NETFX];
    assert!(!installed(
        netfx,
        &x64().key(RegistryView::Bits64, NETFX_KEY)
    ));
    let machine = x64().value(
        RegistryView::Bits64,
        NETFX_KEY,
        "Version",
        RegistryValue::Other,
    );
    assert!(installed(netfx, &machine));

    // Key present.
    let manifest = parse_edited(|m| {
        component(m, NETFX)["detect"] = json!({
            "type": "registry", "key": format!("HKLM\\{NETFX_KEY}"), "view": "64"
        });
    })
    .unwrap();
    let netfx = &manifest.components[NETFX];
    assert!(!installed(netfx, &x64()));
    assert!(installed(
        netfx,
        &x64().key(RegistryView::Bits64, NETFX_KEY)
    ));

    // A string, compared exactly, and a dotted version.
    let manifest = parse_edited(|m| {
        component(m, NETFX)["detect"] = json!({"type": "all", "rules": [
            {"type": "registry", "key": format!("HKLM\\{NETFX_KEY}"), "view": "64",
             "value": "SP", "equals": "1"},
            {"type": "registry", "key": format!("HKLM\\{NETFX_KEY}"), "view": "64",
             "value": "Version", "min_version": "3.5.30729"}
        ]});
    })
    .unwrap();
    let netfx = &manifest.components[NETFX];
    let machine = |sp: &str, version: &str| {
        x64()
            .value(
                RegistryView::Bits64,
                NETFX_KEY,
                "SP",
                RegistryValue::String(sp.into()),
            )
            .value(
                RegistryView::Bits64,
                NETFX_KEY,
                "Version",
                RegistryValue::String(version.into()),
            )
    };
    assert!(installed(netfx, &machine("1", "3.5.30729.4926")));
    assert!(installed(netfx, &machine("1", " 3.5.30729 ")));
    assert!(!installed(netfx, &machine("1", "3.5.21022.8")));
    assert!(!installed(netfx, &machine("1", "3.5 SP1")));
    assert!(!installed(netfx, &machine("01", "3.5.30729.4926")));
}

#[test]
fn directx_needs_every_file() {
    let manifest = fixture();
    let directx = &manifest.components[DIRECTX];
    let all = x64()
        .file(r"C:\Windows\System32\d3dx9_43.dll", None)
        .file(r"C:\Windows\SysWOW64\d3dx9_43.dll", None)
        .file(r"C:\Windows\System32\xaudio2_7.dll", None);
    assert!(!installed(directx, &all));
    let all = all.file(r"C:\Windows\SysWOW64\XAudio2_7.dll", None);
    assert!(installed(directx, &all));
}

#[test]
fn file_versions() {
    let manifest = parse_edited(|m| {
        component(m, DIRECTX)["detect"] = json!({
            "type": "file", "path": r"{system32}\d3dx9_43.dll", "min_version": "9.29.952.3111"
        });
    })
    .unwrap();
    let directx = &manifest.components[DIRECTX];
    let with = |version: Option<&str>| x64().file(r"C:\Windows\System32\d3dx9_43.dll", version);
    assert!(installed(directx, &with(Some("9.29.952.3111"))));
    assert!(installed(directx, &with(Some("10.0.0.0"))));
    assert!(!installed(directx, &with(Some("9.29.952.3110"))));
    // A file without a version resource is not new enough.
    assert!(!installed(directx, &with(None)));
}

#[test]
fn dotnet_takes_the_same_major_at_or_above_the_minimum() {
    let manifest = fixture();
    let dotnet = &manifest.components[DOTNET];
    let root = r"C:\Program Files\dotnet";

    assert!(installed(dotnet, &x64().dotnet(root, DESKTOP, "8.0.0")));
    assert!(installed(dotnet, &x64().dotnet(root, DESKTOP, "8.0.25")));
    assert!(installed(
        dotnet,
        &x64()
            .dotnet(root, DESKTOP, "9.0.1")
            .dotnet(root, DESKTOP, "8.0.34")
    ));
    // Another major does not do.
    assert!(!installed(dotnet, &x64().dotnet(root, DESKTOP, "9.0.1")));
    assert!(!installed(dotnet, &x64().dotnet(root, DESKTOP, "7.0.20")));
    // Nor does another framework, or a preview.
    assert!(!installed(
        dotnet,
        &x64().dotnet(root, "Microsoft.NETCore.App", "8.0.25")
    ));
    assert!(!installed(
        dotnet,
        &x64().dotnet(root, DESKTOP, "8.0.0-rc.2.23479.6")
    ));
    // Nor a version folder an uninstall left empty.
    assert!(!installed(
        dotnet,
        &x64().folder(&format!(r"{root}\shared\{DESKTOP}\8.0.25"))
    ));
}

#[test]
fn dotnet_looks_where_its_architecture_installs() {
    let dotnet_for = |arch: &str| {
        parse_edited(|m| component(m, DOTNET)["arch"] = json!(arch))
            .unwrap()
            .components[DOTNET]
            .clone()
    };
    let (x86, x64_runtime, arm64_runtime) =
        (dotnet_for("x86"), dotnet_for("x64"), dotnet_for("arm64"));
    let native_root = r"C:\Program Files\dotnet";
    let x86_root = r"C:\Program Files (x86)\dotnet";
    let x64_on_arm64_root = r"C:\Program Files\dotnet\x64";

    // x64 Windows.
    let machine = x64().dotnet(x86_root, DESKTOP, "8.0.25");
    assert!(installed(&x86, &machine));
    assert!(!installed(&x64_runtime, &machine));
    let machine = x64().dotnet(native_root, DESKTOP, "8.0.25");
    assert!(!installed(&x86, &machine));
    assert!(installed(&x64_runtime, &machine));

    // ARM64 Windows: the native folder is ARM64, and x64 has its own below
    // it.
    let machine = arm64().dotnet(native_root, DESKTOP, "8.0.25");
    assert!(installed(&arm64_runtime, &machine));
    assert!(!installed(&x64_runtime, &machine));
    let machine = arm64().dotnet(x64_on_arm64_root, DESKTOP, "8.0.25");
    assert!(installed(&x64_runtime, &machine));
    assert!(!installed(&arm64_runtime, &machine));
    let machine = arm64().dotnet(x86_root, DESKTOP, "8.0.25");
    assert!(installed(&x86, &machine));
}

#[test]
fn components_only_apply_to_their_machines() {
    for native in [NativeArch::X64, NativeArch::Arm64] {
        assert!(applies_to(Arch::Any, native));
        assert!(applies_to(Arch::X86, native));
        assert!(applies_to(Arch::X64, native));
    }
    assert!(!applies_to(Arch::Arm64, NativeArch::X64));
    assert!(applies_to(Arch::Arm64, NativeArch::Arm64));

    let manifest = parse_edited(|m| component(m, VC_X64)["arch"] = json!("arm64")).unwrap();
    let arm64_vc = &manifest.components[VC_X64];
    assert!(matches!(
        is_installed(arm64_vc, &x64()),
        Err(DetectError::NotForThisMachine {
            arch: Arch::Arm64,
            native: NativeArch::X64
        })
    ));
    assert!(!installed(arm64_vc, &arm64()));
}

#[test]
fn command_rules_run_only_existing_programs() {
    let manifest: Manifest = parse_edited(|m| {
        component(m, NETFX)["detect"] = json!({
            "type": "command",
            "program": r"{system32}\check.exe",
            "args": ["/feature", "NetFx3"],
            "exit_installed": [0, 3010]
        });
    })
    .unwrap();
    let netfx = &manifest.components[NETFX];
    let program = r"C:\Windows\System32\check.exe";

    let machine = x64();
    assert!(!installed(netfx, &machine));
    assert!(
        machine.runs.borrow().is_empty(),
        "a missing program was run"
    );

    for (code, expected) in [(0, true), (3010, true), (1, false)] {
        let machine = x64().program(program, code);
        assert_eq!(installed(netfx, &machine), expected, "exit code {code}");
        assert_eq!(
            *machine.runs.borrow(),
            [(
                program.to_owned(),
                vec!["/feature".to_owned(), "NetFx3".to_owned()]
            )]
        );
    }
}

#[test]
fn a_question_without_an_answer_is_an_error() {
    let manifest = fixture();
    let vc64 = &manifest.components[VC_X64];
    let machine = with_vc(x64(), 35211).deny(VC_KEY_X64);
    let error = is_installed(vc64, &machine).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(r"HKLM\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64")
            && message.contains("64-bit view")
            && message.contains("access denied"),
        "{message}"
    );

    // With the System32 files there, only the denied SysWOW64 ones decide.
    // (Without them, the first missing file settles "not installed".)
    let directx = &manifest.components[DIRECTX];
    let machine = x64()
        .file(r"C:\Windows\System32\d3dx9_43.dll", None)
        .file(r"C:\Windows\System32\xaudio2_7.dll", None)
        .deny(r"C:\Windows\SysWOW64");
    let error = is_installed(directx, &machine).unwrap_err();
    assert!(error.to_string().contains("SysWOW64"), "{error}");
    assert!(!installed(directx, &x64().deny(r"C:\Windows\SysWOW64")));

    let dotnet = &manifest.components[DOTNET];
    assert!(is_installed(dotnet, &x64().deny(r"C:\Program Files\dotnet")).is_err());
}

#[test]
fn all_and_any_decide_around_errors() {
    let denied = r"SOFTWARE\Denied";
    let manifest_with = |kind: &str| {
        parse_edited(|m| {
            component(m, NETFX)["detect"] = json!({"type": kind, "rules": [
                {"type": "registry", "key": format!("HKLM\\{denied}"), "view": "64"},
                {"type": "registry", "key": format!("HKLM\\{NETFX_KEY}"), "view": "64"}
            ]});
        })
        .unwrap()
    };
    let (all, any) = (manifest_with("all"), manifest_with("any"));
    let (all, any) = (&all.components[NETFX], &any.components[NETFX]);
    let present = x64().key(RegistryView::Bits64, NETFX_KEY).deny(denied);
    let absent = x64().deny(denied);

    // A true rule settles `any`, a false one settles `all`, whatever the
    // denied rule would have said.
    assert!(installed(any, &present));
    assert!(!installed(all, &absent));
    // Otherwise the denied rule is the answer.
    assert!(is_installed(all, &present).is_err());
    assert!(is_installed(any, &absent).is_err());
}
