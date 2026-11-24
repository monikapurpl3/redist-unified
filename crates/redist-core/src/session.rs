//! One run of the installer.
//!
//! 1. Detect which components that belong on this machine are missing.
//! 2. Download those in parallel, each from its mirrors and in parts (see
//!    [`download`](crate::download)), and check each file's SHA-256 on disk.
//! 3. Install them one at a time, in manifest order (Windows Installer runs
//!    one installation at a time).
//! 4. Check with detection that each installation took.
//!
//! The first failure stops the run: nothing after it is installed, and the
//! run never reports success. The world outside (downloads, running
//! installers, waiting, showing progress) comes in through [`Fetcher`],
//! [`Runner`] and [`Reporter`], so tests can drive the whole flow.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use sha2::Digest;

use crate::detection::{self, DetectError, Machine};
pub use crate::download::Fetcher;
use crate::download::fetch_file;
use crate::log::Log;
use crate::manifest::{Component, Download, ExitOutcome, Installer, KnownFolder, Manifest, Sha256};

/// How often an installer is tried while another installation is running.
pub const BUSY_ATTEMPTS: u32 = 10;

/// How long to wait between those tries.
pub const BUSY_WAIT: Duration = Duration::from_secs(30);

/// How many downloads run at once.
pub const PARALLEL_DOWNLOADS: usize = 4;

/// Runs installers.
pub trait Runner {
    /// Runs `program` with `args`, in the folder `dir`, and waits for it.
    /// Returns its exit code.
    fn run(&self, program: &Path, args: &[String], dir: &Path) -> io::Result<u32>;

    /// Waits before trying again while another installation is running.
    fn wait(&self, duration: Duration);
}

/// Shows the user what is happening.
pub trait Reporter: Sync {
    fn report(&self, event: Event<'_>);
}

/// Something the user may want to see.
#[derive(Clone, Copy, Debug)]
pub enum Event<'a> {
    /// A component that belongs on this machine has been checked.
    Checked {
        component: &'a Component,
        installed: bool,
    },
    /// Downloads start: `count` files.
    Downloading { count: usize },
    /// Part of a download arrived: `received` of `total` bytes.
    Progress {
        component: &'a Component,
        received: u64,
        total: u64,
    },
    /// A download finished and its hash matched.
    Downloaded { component: &'a Component },
    /// An installer starts.
    Installing { component: &'a Component },
    /// Another installation is running; trying again after a wait.
    Waiting {
        component: &'a Component,
        attempt: u32,
    },
    /// A component is done.
    Done {
        component: &'a Component,
        outcome: Outcome,
    },
}

/// What happened to a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It is for an architecture this machine does not use.
    NotForThisMachine,
    /// It was already installed.
    Present,
    /// It is missing; only reported when checking.
    Missing,
    /// This run installed it.
    Installed,
    /// This run installed it, and it finishes when Windows restarts.
    InstalledAfterRestart,
    /// Its installer found it, or a newer version, already there.
    InstallerFoundIt,
}

/// What a run did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Every component considered, in manifest order, by id.
    pub results: Vec<(String, Outcome)>,
    /// Whether Windows must restart to finish.
    pub restart_needed: bool,
}

/// Choices for a run.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    /// Only this component, by id.
    pub only: Option<&'a str>,
    /// Detect and report, but download and install nothing.
    pub check_only: bool,
}

/// Why a run stopped.
#[derive(Debug)]
pub enum SessionError {
    /// `only` names no component.
    UnknownComponent(String),
    /// `only` names a component for another architecture.
    NotForThisMachine(String),
    /// Detection could not decide.
    Detect { name: String, source: DetectError },
    /// The work folder could not be prepared.
    WorkFolder(io::Error),
    /// A download failed.
    Download { name: String, source: io::Error },
    /// A download is not the file the manifest names.
    HashMismatch {
        name: String,
        expected: Sha256,
        actual: Sha256,
    },
    /// An installer could not be started.
    Start {
        name: String,
        program: PathBuf,
        source: io::Error,
    },
    /// An installer failed.
    Failed { name: String, code: u32 },
    /// Another installation kept running.
    Busy { name: String },
    /// An installer reported success, but the component is still missing.
    StillMissing { name: String },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::UnknownComponent(id) => {
                write!(f, "The manifest has no component {id:?}.")
            }
            SessionError::NotForThisMachine(id) => {
                write!(f, "The component {id:?} is not for this computer.")
            }
            SessionError::Detect { name, source } => {
                write!(f, "Could not check whether {name} is installed: {source}.")
            }
            SessionError::WorkFolder(source) => {
                write!(f, "Could not prepare the folder for downloads: {source}.")
            }
            SessionError::Download { name, source } => write!(
                f,
                "Could not download {name}: {source}. Check the internet connection and try again."
            ),
            SessionError::HashMismatch {
                name,
                expected,
                actual,
            } => write!(
                f,
                "The download of {name} is not the expected file, so it was not run \
                 (SHA-256 {actual}, expected {expected})."
            ),
            SessionError::Start {
                name,
                program,
                source,
            } => write!(
                f,
                "Could not start the installer of {name} ({}): {source}.",
                program.display()
            ),
            SessionError::Failed { name, code } => write!(
                f,
                "The installer of {name} failed with exit code {code} ({code:#010x})."
            ),
            SessionError::Busy { name } => write!(
                f,
                "Another installation kept running, so {name} could not be installed. \
                 Try again once it has finished."
            ),
            SessionError::StillMissing { name } => write!(
                f,
                "The installer of {name} reported success, but {name} is still not installed."
            ),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Detect { source, .. } => Some(source),
            SessionError::WorkFolder(source)
            | SessionError::Download { source, .. }
            | SessionError::Start { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Everything a run needs from outside.
pub struct Session<'a> {
    pub machine: &'a dyn Machine,
    pub fetcher: &'a dyn Fetcher,
    pub runner: &'a dyn Runner,
    pub reporter: &'a dyn Reporter,
    pub log: &'a Log,
    /// A folder only administrators can write to. Downloads and unpacked
    /// installers go here, and installers run from here.
    pub work_dir: &'a Path,
}

impl Session<'_> {
    /// Runs the installer for `manifest`.
    pub fn run(&self, manifest: &Manifest, options: &Options<'_>) -> Result<Summary, SessionError> {
        let native = self.machine.native_arch();
        self.log.line(format_args!(
            "Manifest version {}, issued {}; Windows is {native:?}",
            manifest.version, manifest.issued
        ));

        let selected: Vec<&Component> = match options.only {
            Some(id) => {
                let component = manifest
                    .component(id)
                    .ok_or_else(|| SessionError::UnknownComponent(id.to_owned()))?;
                if !detection::applies_to(component.arch, native) {
                    return Err(SessionError::NotForThisMachine(id.to_owned()));
                }
                vec![component]
            }
            None => manifest.components.iter().collect(),
        };

        // Detect.
        let mut results = Vec::new();
        for component in selected {
            let outcome = if !detection::applies_to(component.arch, native) {
                Outcome::NotForThisMachine
            } else {
                let installed =
                    detection::is_installed(component, self.machine).map_err(|source| {
                        self.log
                            .line(format_args!("{}: detection failed: {source}", component.id));
                        SessionError::Detect {
                            name: component.name.clone(),
                            source,
                        }
                    })?;
                self.reporter.report(Event::Checked {
                    component,
                    installed,
                });
                if installed {
                    Outcome::Present
                } else {
                    Outcome::Missing
                }
            };
            self.log.line(format_args!("{}: {outcome:?}", component.id));
            results.push((component, outcome));
        }

        let missing: Vec<&Component> = results
            .iter()
            .filter(|(_, outcome)| *outcome == Outcome::Missing)
            .map(|(component, _)| *component)
            .collect();
        if !options.check_only && !missing.is_empty() {
            let files = self.download(&missing)?;
            for (component, outcome) in &mut results {
                if *outcome == Outcome::Missing {
                    *outcome = self.install(component, files.get(component.id.as_str()))?;
                    self.reporter.report(Event::Done {
                        component,
                        outcome: *outcome,
                    });
                }
            }
        }

        let restart_needed = results
            .iter()
            .any(|(_, outcome)| *outcome == Outcome::InstalledAfterRestart);
        self.log.line(format_args!(
            "Finished{}",
            if restart_needed {
                "; Windows must restart"
            } else {
                ""
            }
        ));
        Ok(Summary {
            results: results
                .into_iter()
                .map(|(component, outcome)| (component.id.clone(), outcome))
                .collect(),
            restart_needed,
        })
    }

    /// Downloads every missing component that has a download, a few at a
    /// time. Waits for all of them, then fails with the first failure in
    /// manifest order, if any.
    fn download<'m>(
        &self,
        missing: &[&'m Component],
    ) -> Result<HashMap<&'m str, PathBuf>, SessionError> {
        let jobs: Vec<(&Component, &Download)> = missing
            .iter()
            .filter_map(|component| component.download.as_ref().map(|d| (*component, d)))
            .collect();
        if jobs.is_empty() {
            return Ok(HashMap::new());
        }
        let dir = self.work_dir.join("downloads");
        fs::create_dir_all(&dir).map_err(SessionError::WorkFolder)?;
        self.reporter
            .report(Event::Downloading { count: jobs.len() });

        let next = AtomicUsize::new(0);
        let done: Mutex<Vec<Option<Result<PathBuf, SessionError>>>> =
            Mutex::new((0..jobs.len()).map(|_| None).collect());
        // Only Sync parts go to the download threads; the machine and the
        // runner stay on this one.
        let (fetcher, reporter, log) = (self.fetcher, self.reporter, self.log);
        thread::scope(|scope| {
            for _ in 0..PARALLEL_DOWNLOADS.min(jobs.len()) {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(component, download)) = jobs.get(index) else {
                            break;
                        };
                        let result = fetch_one(fetcher, reporter, log, component, download, &dir);
                        let mut done = done.lock().unwrap_or_else(|p| p.into_inner());
                        done[index] = Some(result);
                    }
                });
            }
        });

        let mut files = HashMap::new();
        let done = done.into_inner().unwrap_or_else(|p| p.into_inner());
        for (&(component, _), result) in jobs.iter().zip(done) {
            let path = result.expect("every download job ran")?;
            files.insert(component.id.as_str(), path);
        }
        Ok(files)
    }

    fn install(
        &self,
        component: &Component,
        file: Option<&PathBuf>,
    ) -> Result<Outcome, SessionError> {
        self.reporter.report(Event::Installing { component });
        self.log.line(format_args!("{}: installing", component.id));
        let downloaded = || file.expect("components with a download were downloaded");
        let outcome = match &component.installer {
            Installer::Exe { args } => self.run_installer(component, downloaded(), args)?,
            Installer::Msi { args } => {
                let msiexec = self.system_program(component, "msiexec.exe")?;
                let mut all = vec!["/i".to_owned(), downloaded().display().to_string()];
                all.extend(args.iter().cloned());
                self.run_installer(component, &msiexec, &all)?
            }
            Installer::Extract {
                extract_args,
                run,
                args,
            } => {
                let dir = self.work_dir.join(&component.id);
                fs::create_dir(&dir).map_err(SessionError::WorkFolder)?;
                let dir_text = dir.display().to_string();
                let extract_args: Vec<String> = extract_args
                    .iter()
                    .map(|arg| arg.replace("{dir}", &dir_text))
                    .collect();
                self.log.line(format_args!(
                    "{}: unpacking {} {extract_args:?}",
                    component.id,
                    downloaded().display()
                ));
                let code = self.start(component, downloaded(), &extract_args)?;
                self.log.line(format_args!(
                    "{}: unpacking ended with exit code {code}",
                    component.id
                ));
                if code != 0 {
                    return Err(SessionError::Failed {
                        name: component.name.clone(),
                        code,
                    });
                }
                let program = PathBuf::from(format!("{dir_text}\\{}", run.as_str()));
                self.run_installer(component, &program, args)?
            }
            Installer::WindowsFeature { feature } => {
                let dism = self.system_program(component, "dism.exe")?;
                let args = [
                    "/Online".to_owned(),
                    "/Enable-Feature".to_owned(),
                    format!("/FeatureName:{feature}"),
                    "/All".to_owned(),
                    "/NoRestart".to_owned(),
                ];
                self.run_installer(component, &dism, &args)?
            }
        };

        match outcome {
            ExitOutcome::Installed => {
                // An installer can report success and still not install,
                // for example with the wrong silent arguments.
                let present =
                    detection::is_installed(component, self.machine).map_err(|source| {
                        SessionError::Detect {
                            name: component.name.clone(),
                            source,
                        }
                    })?;
                self.log.line(format_args!(
                    "{}: detected after install: {present}",
                    component.id
                ));
                if present {
                    Ok(Outcome::Installed)
                } else {
                    Err(SessionError::StillMissing {
                        name: component.name.clone(),
                    })
                }
            }
            // What these installed may only show after the restart.
            ExitOutcome::RebootNeeded => Ok(Outcome::InstalledAfterRestart),
            ExitOutcome::AlreadyInstalled => Ok(Outcome::InstallerFoundIt),
            ExitOutcome::RetryLater | ExitOutcome::Failed => {
                unreachable!("run_installer returns these as errors")
            }
        }
    }

    /// Runs an installer, trying again while another installation is
    /// running. Returns a successful outcome or an error.
    fn run_installer(
        &self,
        component: &Component,
        program: &Path,
        args: &[String],
    ) -> Result<ExitOutcome, SessionError> {
        let mut attempt = 1;
        loop {
            self.log.line(format_args!(
                "{}: running {} {args:?}",
                component.id,
                program.display()
            ));
            let code = self.start(component, program, args)?;
            let outcome = component.exit_codes.classify(code);
            self.log.line(format_args!(
                "{}: exit code {code} ({code:#010x}): {outcome:?}",
                component.id
            ));
            match outcome {
                ExitOutcome::RetryLater if attempt < BUSY_ATTEMPTS => {
                    self.reporter.report(Event::Waiting { component, attempt });
                    self.runner.wait(BUSY_WAIT);
                    attempt += 1;
                }
                ExitOutcome::RetryLater => {
                    return Err(SessionError::Busy {
                        name: component.name.clone(),
                    });
                }
                ExitOutcome::Failed => {
                    return Err(SessionError::Failed {
                        name: component.name.clone(),
                        code,
                    });
                }
                success => return Ok(success),
            }
        }
    }

    fn start(
        &self,
        component: &Component,
        program: &Path,
        args: &[String],
    ) -> Result<u32, SessionError> {
        self.runner
            .run(program, args, self.work_dir)
            .map_err(|source| {
                self.log.line(format_args!(
                    "{}: could not start {}: {source}",
                    component.id,
                    program.display()
                ));
                SessionError::Start {
                    name: component.name.clone(),
                    program: program.to_owned(),
                    source,
                }
            })
    }

    /// A program in System32, found through Windows, never through `PATH`.
    fn system_program(&self, component: &Component, name: &str) -> Result<PathBuf, SessionError> {
        let system32 = self
            .machine
            .known_folder(KnownFolder::System32)
            .map_err(|source| SessionError::Start {
                name: component.name.clone(),
                program: PathBuf::from(name),
                source,
            })?;
        Ok(PathBuf::from(format!(
            "{}\\{name}",
            system32.display().to_string().trim_end_matches('\\')
        )))
    }
}

/// Downloads one component and checks the file's SHA-256 on disk, which is
/// the file that will run. A file with the wrong hash is deleted.
fn fetch_one(
    fetcher: &dyn Fetcher,
    reporter: &dyn Reporter,
    log: &Log,
    component: &Component,
    download: &Download,
    dir: &Path,
) -> Result<PathBuf, SessionError> {
    let extension = match component.installer {
        Installer::Msi { .. } => "msi",
        _ => "exe",
    };
    let dest = dir.join(format!("{}.{extension}", component.id));
    let mirrors: Vec<&str> = download.urls.iter().map(|url| url.as_str()).collect();
    log.line(format_args!(
        "{}: downloading {} bytes to {} from {}",
        component.id,
        download.size,
        dest.display(),
        mirrors.join(", then ")
    ));
    let failed = |source: io::Error| {
        log.line(format_args!("{}: download failed: {source}", component.id));
        SessionError::Download {
            name: component.name.clone(),
            source,
        }
    };
    let progress = |received| {
        reporter.report(Event::Progress {
            component,
            received,
            total: download.size,
        });
    };
    fetch_file(fetcher, download, &dest, log, &component.id, &progress).map_err(failed)?;
    let actual = sha256_of(&dest).map_err(failed)?;
    if actual != download.sha256 {
        log.line(format_args!(
            "{}: SHA-256 {actual}, expected {}; deleting it",
            component.id, download.sha256
        ));
        let _ = fs::remove_file(&dest);
        return Err(SessionError::HashMismatch {
            name: component.name.clone(),
            expected: download.sha256,
            actual,
        });
    }
    log.line(format_args!(
        "{}: downloaded, SHA-256 matches",
        component.id
    ));
    reporter.report(Event::Downloaded { component });
    Ok(dest)
}

/// The SHA-256 of a file.
pub fn sha256_of(path: &Path) -> io::Result<Sha256> {
    let mut file = File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256::from_bytes(hasher.finalize().into()))
}
