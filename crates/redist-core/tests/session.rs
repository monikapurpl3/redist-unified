//! A whole run against a fake machine, fake downloads and fake installers.

mod common;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{
    DESKTOP, DIRECTX, DOTNET, FakeMachine, NETFX_KEY, VC_X64, VC_X86, component, parse_edited,
    with_vc, x64,
};
use redist_core::detection::RegistryValue;
use redist_core::log::Log;
use redist_core::manifest::{HttpsUrl, Manifest, RegistryView};
use redist_core::session::{
    BUSY_ATTEMPTS, BUSY_WAIT, Event, Fetcher, Options, Outcome, Reporter, Runner, Session,
    SessionError, Summary,
};
use serde_json::json;
use sha2::Digest;

const WORK: &str = "redist-session-test";

/// The bytes each fake download serves, by component index.
fn payload(index: usize) -> Vec<u8> {
    format!("installer number {index}").into_bytes()
}

fn sha256_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The fixture, with each download's hash set to its fake payload's.
fn manifest_with(edit: impl FnOnce(&mut serde_json::Value)) -> Manifest {
    parse_edited(|m| {
        for index in [VC_X64, VC_X86, DIRECTX, DOTNET] {
            component(m, index)["sha256"] = json!(sha256_hex(&payload(index)));
        }
        edit(m);
    })
    .unwrap()
}

fn manifest() -> Manifest {
    manifest_with(|_| {})
}

/// Serves `payload` for each fixture URL, or fails for the ones in `fail`.
struct FakeFetcher {
    files: HashMap<String, Vec<u8>>,
    fail: Vec<String>,
    fetched: Mutex<Vec<String>>,
}

impl FakeFetcher {
    fn new(manifest: &Manifest) -> Self {
        let files = manifest
            .components
            .iter()
            .enumerate()
            .filter_map(|(index, c)| {
                c.download
                    .as_ref()
                    .map(|d| (d.url.as_str().to_owned(), payload(index)))
            })
            .collect();
        FakeFetcher {
            files,
            fail: Vec::new(),
            fetched: Mutex::new(Vec::new()),
        }
    }

    fn fetched(&self) -> Vec<String> {
        let mut fetched = self.fetched.lock().unwrap().clone();
        fetched.sort();
        fetched
    }
}

impl Fetcher for FakeFetcher {
    fn fetch(
        &self,
        url: &HttpsUrl,
        dest: &Path,
        progress: &dyn Fn(u64, Option<u64>),
    ) -> io::Result<()> {
        self.fetched.lock().unwrap().push(url.as_str().to_owned());
        if self.fail.iter().any(|f| f == url.as_str()) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
        }
        let bytes = &self.files[url.as_str()];
        let total = bytes.len() as u64;
        progress(0, Some(total));
        std::fs::File::create_new(dest)?.write_all(bytes)?;
        progress(total, Some(total));
        Ok(())
    }
}

type Effect<'m> = Box<dyn Fn(&FakeMachine) + 'm>;

/// Answers each program (by file name, ignoring case) with scripted exit
/// codes, in order; the last one repeats. An effect can change the machine,
/// like a real installer.
struct FakeRunner<'m> {
    machine: &'m FakeMachine,
    scripts: RefCell<HashMap<String, VecDeque<u32>>>,
    effects: HashMap<String, Effect<'m>>,
    unstartable: Vec<String>,
    runs: RefCell<Vec<(String, Vec<String>, PathBuf)>>,
    waits: RefCell<Vec<Duration>>,
}

impl<'m> FakeRunner<'m> {
    fn new(machine: &'m FakeMachine) -> Self {
        FakeRunner {
            machine,
            scripts: RefCell::new(HashMap::new()),
            effects: HashMap::new(),
            unstartable: Vec::new(),
            runs: RefCell::new(Vec::new()),
            waits: RefCell::new(Vec::new()),
        }
    }

    fn codes(self, program: &str, codes: &[u32]) -> Self {
        self.scripts
            .borrow_mut()
            .insert(program.to_lowercase(), codes.iter().copied().collect());
        self
    }

    fn effect(mut self, program: &str, effect: impl Fn(&FakeMachine) + 'm) -> Self {
        self.effects
            .insert(program.to_lowercase(), Box::new(effect));
        self
    }

    /// Programs run, by file name.
    fn programs(&self) -> Vec<String> {
        self.runs
            .borrow()
            .iter()
            .map(|(program, _, _)| file_name(program))
            .collect()
    }

    fn run_of(&self, name: &str) -> (String, Vec<String>, PathBuf) {
        self.runs
            .borrow()
            .iter()
            .find(|(program, _, _)| file_name(program).eq_ignore_ascii_case(name))
            .cloned()
            .unwrap_or_else(|| panic!("{name} did not run"))
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap().to_owned()
}

impl Runner for FakeRunner<'_> {
    fn run(&self, program: &Path, args: &[String], dir: &Path) -> io::Result<u32> {
        let program = program.to_string_lossy().into_owned();
        let name = file_name(&program).to_lowercase();
        self.runs
            .borrow_mut()
            .push((program, args.to_vec(), dir.to_owned()));
        if self.unstartable.contains(&name) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "not found"));
        }
        let code = {
            let mut scripts = self.scripts.borrow_mut();
            let codes = scripts.entry(name.clone()).or_insert_with(|| [0].into());
            if codes.len() > 1 {
                codes.pop_front().unwrap()
            } else {
                codes[0]
            }
        };
        if let Some(effect) = self.effects.get(&name) {
            effect(self.machine);
        }
        Ok(code)
    }

    fn wait(&self, duration: Duration) {
        self.waits.borrow_mut().push(duration);
    }
}

/// Records events as short strings.
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Reporter for Recorder {
    fn report(&self, event: Event<'_>) {
        let line = match event {
            Event::Checked {
                component,
                installed,
            } => format!("checked {} {installed}", component.id),
            Event::Downloading { count } => format!("downloading {count}"),
            Event::Progress { .. } => return,
            Event::Downloaded { component } => format!("downloaded {}", component.id),
            Event::Installing { component } => format!("installing {}", component.id),
            Event::Waiting { component, attempt } => format!("waiting {} {attempt}", component.id),
            Event::Done { component, outcome } => format!("done {} {outcome:?}", component.id),
        };
        self.0.lock().unwrap().push(line);
    }
}

impl Recorder {
    fn events(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// A log in memory.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Buffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// A fresh work folder, deleted afterwards.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{WORK}-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        WorkDir(dir)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Run {
    result: Result<Summary, SessionError>,
    events: Vec<String>,
    log: String,
}

fn run(
    manifest: &Manifest,
    machine: &FakeMachine,
    fetcher: &FakeFetcher,
    runner: &FakeRunner<'_>,
    work: &WorkDir,
    options: Options<'_>,
) -> Run {
    let recorder = Recorder::default();
    let buffer = Buffer::default();
    let log = Log::new(buffer.clone());
    let session = Session {
        machine,
        fetcher,
        runner,
        reporter: &recorder,
        log: &log,
        work_dir: &work.0,
    };
    let result = session.run(manifest, &options);
    Run {
        result,
        events: recorder.events(),
        log: buffer.text(),
    }
}

/// A runner whose installers install what the fixture's rules detect.
fn working_installers(machine: &FakeMachine) -> FakeRunner<'_> {
    use RegistryView::{Bits32, Bits64};
    FakeRunner::new(machine)
        .effect("vcredist-2015-2022-x64.exe", |m| {
            m.set_value(
                Bits64,
                common::VC_KEY_X64,
                "Installed",
                RegistryValue::Dword(1),
            );
            m.set_value(
                Bits64,
                common::VC_KEY_X64,
                "Bld",
                RegistryValue::Dword(35211),
            );
        })
        .effect("vcredist-2015-2022-x86.exe", |m| {
            m.set_value(
                Bits32,
                common::VC_KEY_X86,
                "Installed",
                RegistryValue::Dword(1),
            );
            m.set_value(
                Bits32,
                common::VC_KEY_X86,
                "Bld",
                RegistryValue::Dword(35211),
            );
        })
        .effect("DXSETUP.exe", |m| {
            for dir in [r"C:\Windows\System32", r"C:\Windows\SysWOW64"] {
                m.add_file(&format!(r"{dir}\d3dx9_43.dll"), None);
                m.add_file(&format!(r"{dir}\xaudio2_7.dll"), None);
            }
        })
        .effect("dotnet-desktop-8-x64.exe", |m| {
            m.add_dotnet(r"C:\Program Files\dotnet", DESKTOP, "8.0.11");
        })
        .effect("dism.exe", |m| {
            m.set_value(Bits64, NETFX_KEY, "Install", RegistryValue::Dword(1));
        })
}

fn outcomes(summary: &Summary) -> Vec<(&str, Outcome)> {
    summary
        .results
        .iter()
        .map(|(id, outcome)| (id.as_str(), *outcome))
        .collect()
}

#[test]
fn installs_everything_missing_in_order() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine).codes("dism.exe", &[3010]);
    let work = WorkDir::new("everything");

    let run = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    );
    let summary = run.result.unwrap();

    assert_eq!(
        outcomes(&summary),
        [
            ("vcredist-2015-2022-x64", Outcome::Installed),
            ("vcredist-2015-2022-x86", Outcome::Installed),
            ("directx-jun2010", Outcome::Installed),
            ("dotnet-desktop-8-x64", Outcome::Installed),
            ("netfx35", Outcome::InstalledAfterRestart),
        ]
    );
    assert!(summary.restart_needed);

    // Every download once; the Windows feature has none.
    assert_eq!(fetcher.fetched().len(), 4);
    // One at a time, in manifest order; DirectX unpacks first.
    assert_eq!(
        runner.programs(),
        [
            "vcredist-2015-2022-x64.exe",
            "vcredist-2015-2022-x86.exe",
            "directx-jun2010.exe",
            "DXSETUP.exe",
            "dotnet-desktop-8-x64.exe",
            "dism.exe",
        ]
    );
    // Installers run from the work folder.
    assert!(
        runner
            .runs
            .borrow()
            .iter()
            .all(|(_, _, dir)| *dir == work.0)
    );
    // Downloads are in the work folder, named after the component.
    let (program, args, _) = runner.run_of("vcredist-2015-2022-x64.exe");
    assert_eq!(
        PathBuf::from(program),
        work.0.join("downloads").join("vcredist-2015-2022-x64.exe")
    );
    assert_eq!(args, ["/install", "/quiet", "/norestart"]);
    assert_eq!(
        std::fs::read(work.0.join(r"downloads\directx-jun2010.exe")).unwrap(),
        payload(DIRECTX)
    );

    // DirectX unpacks into its own folder, then runs from there.
    let unpacked = work.0.join("directx-jun2010");
    let (_, args, _) = runner.run_of("directx-jun2010.exe");
    assert_eq!(
        args,
        ["/Q".to_owned(), format!("/T:{}", unpacked.display())]
    );
    let (program, args, _) = runner.run_of("DXSETUP.exe");
    assert_eq!(program, format!(r"{}\DXSETUP.exe", unpacked.display()));
    assert_eq!(args, ["/silent"]);

    // The feature goes through System32's DISM.
    let (program, args, _) = runner.run_of("dism.exe");
    assert_eq!(program, r"C:\Windows\System32\dism.exe");
    assert_eq!(
        args,
        [
            "/Online",
            "/Enable-Feature",
            "/FeatureName:NetFx3",
            "/All",
            "/NoRestart"
        ]
    );

    assert_eq!(
        &run.events[..6],
        [
            "checked vcredist-2015-2022-x64 false",
            "checked vcredist-2015-2022-x86 false",
            "checked directx-jun2010 false",
            "checked dotnet-desktop-8-x64 false",
            "checked netfx35 false",
            "downloading 4",
        ]
    );
    assert_eq!(
        run.events.last().unwrap(),
        "done netfx35 InstalledAfterRestart"
    );
    assert!(
        run.log
            .contains("netfx35: exit code 3010 (0x00000bc2): RebootNeeded")
    );
    assert!(
        run.log
            .contains("vcredist-2015-2022-x64: downloaded, SHA-256 matches")
    );
    assert!(run.log.contains("Windows must restart"));
}

#[test]
fn present_components_are_left_alone() {
    let manifest = manifest();
    let machine = with_vc(x64(), 40000);
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine);
    let work = WorkDir::new("present");

    let summary = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    )
    .result
    .unwrap();
    assert_eq!(
        &outcomes(&summary)[..2],
        [
            ("vcredist-2015-2022-x64", Outcome::Present),
            ("vcredist-2015-2022-x86", Outcome::Present),
        ]
    );
    assert!(!summary.restart_needed);
    assert!(
        !fetcher
            .fetched()
            .iter()
            .any(|url| url.contains("vc_redist"))
    );
    assert!(!runner.programs().iter().any(|p| p.starts_with("vcredist")));
}

#[test]
fn nothing_missing_means_nothing_to_do() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine);
    let work = WorkDir::new("nothing");
    // Install everything once, then run again.
    run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    )
    .result
    .unwrap();
    let runs_before = runner.programs().len();

    let again = WorkDir::new("nothing-again");
    let summary = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &again,
        Options::default(),
    )
    .result
    .unwrap();
    assert!(summary.results.iter().all(|(_, o)| *o == Outcome::Present));
    assert_eq!(runner.programs().len(), runs_before);
    assert!(!again.0.join("downloads").exists());
}

#[test]
fn checking_downloads_and_installs_nothing() {
    let manifest = manifest();
    let machine = with_vc(x64(), 40000);
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine);
    let work = WorkDir::new("check");

    let options = Options {
        check_only: true,
        ..Options::default()
    };
    let summary = run(&manifest, &machine, &fetcher, &runner, &work, options)
        .result
        .unwrap();
    assert_eq!(
        outcomes(&summary),
        [
            ("vcredist-2015-2022-x64", Outcome::Present),
            ("vcredist-2015-2022-x86", Outcome::Present),
            ("directx-jun2010", Outcome::Missing),
            ("dotnet-desktop-8-x64", Outcome::Missing),
            ("netfx35", Outcome::Missing),
        ]
    );
    assert!(fetcher.fetched().is_empty());
    assert!(runner.programs().is_empty());
}

#[test]
fn only_one_component() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine);
    let work = WorkDir::new("only");

    let options = Options {
        only: Some("dotnet-desktop-8-x64"),
        ..Options::default()
    };
    let summary = run(&manifest, &machine, &fetcher, &runner, &work, options)
        .result
        .unwrap();
    assert_eq!(
        outcomes(&summary),
        [("dotnet-desktop-8-x64", Outcome::Installed)]
    );
    assert_eq!(fetcher.fetched().len(), 1);
    assert_eq!(runner.programs(), ["dotnet-desktop-8-x64.exe"]);

    let options = Options {
        only: Some("vcredist-2008-x64"),
        ..Options::default()
    };
    let result = run(&manifest, &machine, &fetcher, &runner, &work, options).result;
    assert!(matches!(result, Err(SessionError::UnknownComponent(id)) if id == "vcredist-2008-x64"));
}

#[test]
fn components_for_other_machines_are_skipped() {
    let manifest = manifest_with(|m| component(m, VC_X86)["arch"] = json!("arm64"));
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine);
    let work = WorkDir::new("other-arch");

    let summary = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    )
    .result
    .unwrap();
    assert_eq!(
        outcomes(&summary)[1],
        ("vcredist-2015-2022-x86", Outcome::NotForThisMachine)
    );
    assert!(!fetcher.fetched().iter().any(|url| url.contains("x86")));

    let options = Options {
        only: Some("vcredist-2015-2022-x86"),
        ..Options::default()
    };
    let result = run(&manifest, &machine, &fetcher, &runner, &work, options).result;
    assert!(matches!(result, Err(SessionError::NotForThisMachine(_))));
}

#[test]
fn a_wrong_download_is_deleted_and_never_run() {
    let manifest = manifest();
    let machine = x64();
    let mut fetcher = FakeFetcher::new(&manifest);
    let url = manifest.components[DOTNET]
        .download
        .as_ref()
        .unwrap()
        .url
        .as_str()
        .to_owned();
    fetcher.files.insert(url, b"something else".to_vec());
    let runner = working_installers(&machine);
    let work = WorkDir::new("hash");

    let run = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    );
    let error = run.result.unwrap_err();
    assert!(
        matches!(error, SessionError::HashMismatch { .. }),
        "{error}"
    );
    assert!(error.to_string().contains(".NET 8 Desktop Runtime (x64)"));
    assert!(error.to_string().contains("was not run"));
    // Nothing at all runs: the run stops before installing.
    assert!(runner.programs().is_empty());
    assert!(!work.0.join(r"downloads\dotnet-desktop-8-x64.exe").exists());
    assert!(run.log.contains("deleting it"));
}

#[test]
fn a_failed_download_stops_the_run_before_installing() {
    let manifest = manifest();
    let machine = x64();
    let mut fetcher = FakeFetcher::new(&manifest);
    fetcher.fail.push(
        manifest.components[DIRECTX]
            .download
            .as_ref()
            .unwrap()
            .url
            .as_str()
            .to_owned(),
    );
    let runner = working_installers(&machine);
    let work = WorkDir::new("download");

    let error = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    )
    .result
    .unwrap_err();
    assert!(matches!(error, SessionError::Download { .. }));
    assert!(error.to_string().contains("Could not download DirectX"));
    // The other downloads still ran; no installer did.
    assert_eq!(fetcher.fetched().len(), 4);
    assert!(runner.programs().is_empty());
}

#[test]
fn another_installation_is_waited_for() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine).codes("vcredist-2015-2022-x64.exe", &[1618, 1618, 0]);
    let work = WorkDir::new("busy");
    let options = Options {
        only: Some("vcredist-2015-2022-x64"),
        ..Options::default()
    };

    let run = run(&manifest, &machine, &fetcher, &runner, &work, options);
    assert_eq!(
        outcomes(&run.result.unwrap()),
        [("vcredist-2015-2022-x64", Outcome::Installed)]
    );
    assert_eq!(*runner.waits.borrow(), [BUSY_WAIT, BUSY_WAIT]);
    assert!(
        run.events
            .contains(&"waiting vcredist-2015-2022-x64 2".to_owned())
    );
}

#[test]
fn another_installation_that_never_ends_stops_the_run() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine).codes("vcredist-2015-2022-x64.exe", &[1618]);
    let work = WorkDir::new("always-busy");

    let error = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    )
    .result
    .unwrap_err();
    assert!(matches!(error, SessionError::Busy { .. }));
    assert_eq!(runner.programs().len(), BUSY_ATTEMPTS as usize);
    assert_eq!(runner.waits.borrow().len(), BUSY_ATTEMPTS as usize - 1);
}

#[test]
fn a_failed_installer_stops_the_run() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine).codes("vcredist-2015-2022-x86.exe", &[1603]);
    let work = WorkDir::new("failed");

    let run = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    );
    let error = run.result.unwrap_err();
    assert!(
        matches!(&error, SessionError::Failed { code: 1603, .. }),
        "{error}"
    );
    assert!(error.to_string().contains("exit code 1603 (0x00000643)"));
    // The one before it installed; nothing after it ran.
    assert_eq!(
        runner.programs(),
        ["vcredist-2015-2022-x64.exe", "vcredist-2015-2022-x86.exe"]
    );
    assert!(
        run.events
            .contains(&"done vcredist-2015-2022-x64 Installed".to_owned())
    );
}

#[test]
fn an_installer_that_finds_its_runtime_is_not_an_error() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    // 1638 without installing anything: detection is not asked again.
    let runner = FakeRunner::new(&machine).codes("vcredist-2015-2022-x64.exe", &[1638]);
    let work = WorkDir::new("already");
    let options = Options {
        only: Some("vcredist-2015-2022-x64"),
        ..Options::default()
    };

    let summary = run(&manifest, &machine, &fetcher, &runner, &work, options)
        .result
        .unwrap();
    assert_eq!(
        outcomes(&summary),
        [("vcredist-2015-2022-x64", Outcome::InstallerFoundIt)]
    );
}

#[test]
fn success_that_installed_nothing_is_a_failure() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = FakeRunner::new(&machine);
    let work = WorkDir::new("still-missing");

    let error = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    )
    .result
    .unwrap_err();
    assert!(matches!(error, SessionError::StillMissing { .. }));
    assert!(error.to_string().contains("reported success"));
    assert_eq!(runner.programs(), ["vcredist-2015-2022-x64.exe"]);
}

#[test]
fn a_restart_skips_the_check_after_installing() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    // 3010 without installing anything yet, as before a restart.
    let runner = FakeRunner::new(&machine).codes("vcredist-2015-2022-x64.exe", &[3010]);
    let work = WorkDir::new("restart");
    let options = Options {
        only: Some("vcredist-2015-2022-x64"),
        ..Options::default()
    };

    let summary = run(&manifest, &machine, &fetcher, &runner, &work, options)
        .result
        .unwrap();
    assert!(summary.restart_needed);
}

#[test]
fn installers_that_cannot_start_or_unpack() {
    let manifest = manifest();
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let mut runner = working_installers(&machine);
    runner.unstartable.push("vcredist-2015-2022-x64.exe".into());
    let work = WorkDir::new("unstartable");
    let error = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    )
    .result
    .unwrap_err();
    assert!(matches!(error, SessionError::Start { .. }));

    let runner = working_installers(&machine).codes("directx-jun2010.exe", &[1]);
    let work = WorkDir::new("unpack");
    let options = Options {
        only: Some("directx-jun2010"),
        ..Options::default()
    };
    let error = run(&manifest, &machine, &fetcher, &runner, &work, options)
        .result
        .unwrap_err();
    assert!(matches!(error, SessionError::Failed { code: 1, .. }));
    assert_eq!(runner.programs(), ["directx-jun2010.exe"]);
}

#[test]
fn msi_packages_go_through_msiexec() {
    let manifest = manifest_with(|m| {
        component(m, VC_X64)["installer"] = json!({"type": "msi", "args": ["/qn", "/norestart"]});
    });
    let machine = x64();
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine).effect("msiexec.exe", |m| {
        m.set_value(
            RegistryView::Bits64,
            common::VC_KEY_X64,
            "Installed",
            RegistryValue::Dword(1),
        );
        m.set_value(
            RegistryView::Bits64,
            common::VC_KEY_X64,
            "Bld",
            RegistryValue::Dword(35211),
        );
    });
    let work = WorkDir::new("msi");
    let options = Options {
        only: Some("vcredist-2015-2022-x64"),
        ..Options::default()
    };

    run(&manifest, &machine, &fetcher, &runner, &work, options)
        .result
        .unwrap();
    let (program, args, _) = runner.run_of("msiexec.exe");
    assert_eq!(program, r"C:\Windows\System32\msiexec.exe");
    let msi = work.0.join(r"downloads\vcredist-2015-2022-x64.msi");
    assert_eq!(
        args,
        [
            "/i".to_owned(),
            msi.display().to_string(),
            "/qn".into(),
            "/norestart".into()
        ]
    );
    assert!(msi.exists());
}

#[test]
fn detection_that_cannot_decide_stops_the_run() {
    let manifest = manifest();
    let machine = x64().deny(NETFX_KEY);
    let fetcher = FakeFetcher::new(&manifest);
    let runner = working_installers(&machine);
    let work = WorkDir::new("detect");

    let error = run(
        &manifest,
        &machine,
        &fetcher,
        &runner,
        &work,
        Options::default(),
    )
    .result
    .unwrap_err();
    assert!(matches!(error, SessionError::Detect { .. }));
    assert!(error.to_string().contains(".NET Framework 3.5"));
    assert!(fetcher.fetched().is_empty());
}
