//! What a run needs: the manifest, the machine, downloads, installers, a
//! restart, and a work folder with its log. The exes set it up with Windows
//! itself ([`Context::prepare`]); the demo with stand-ins.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use redist_core::detection::Machine;
use redist_core::log::Log;
use redist_core::manifest::Manifest;
use redist_core::session::{Fetcher, Reporter, Runner, Session};
use redist_core::windows::{
    ProcessRunner, WinHttpFetcher, WindowsMachine, create_work_dir, restart_windows,
};

/// Restarts Windows, or says why it could not.
pub type Restart = Box<dyn Fn() -> io::Result<()> + Send + Sync>;

/// Everything a run needs. It can be shared with worker threads.
pub struct Context {
    pub manifest: Manifest,
    pub machine: Box<dyn Machine + Send + Sync>,
    pub fetcher: Box<dyn Fetcher + Send>,
    pub runner: Box<dyn Runner + Send + Sync>,
    pub restart: Restart,
    pub work_dir: PathBuf,
    pub log_path: PathBuf,
    pub log: Log,
}

impl Context {
    /// Reads and checks a manifest. The error is a sentence for the user.
    pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
        let bytes = fs::read(path)
            .map_err(|error| format!("Could not read {}: {error}.", path.display()))?;
        Manifest::parse(&bytes).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Sets up a real run of `program` with `manifest`: Windows itself, and
    /// a new private work folder with its log.
    pub fn prepare(manifest: Manifest, program: &str, version: &str) -> io::Result<Context> {
        Context::new(
            manifest,
            Box::new(WindowsMachine::new()?),
            Box::new(WinHttpFetcher::new()?),
            Box::new(ProcessRunner),
            Box::new(restart_windows),
            create_work_dir()?,
            program,
            version,
        )
    }

    /// Sets up a run from its parts, with a new log in `work_dir`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manifest: Manifest,
        machine: Box<dyn Machine + Send + Sync>,
        fetcher: Box<dyn Fetcher + Send>,
        runner: Box<dyn Runner + Send + Sync>,
        restart: Restart,
        work_dir: PathBuf,
        program: &str,
        version: &str,
    ) -> io::Result<Context> {
        let log_path = work_dir.join(format!("{program}.log"));
        let log = Log::create(&log_path)?;
        log.line(format_args!(
            "{program} {version}; manifest version {}",
            manifest.version
        ));
        Ok(Context {
            manifest,
            machine,
            fetcher,
            runner,
            restart,
            work_dir,
            log_path,
            log,
        })
    }

    /// A session that reports to `reporter`.
    pub fn session<'a>(&'a self, reporter: &'a dyn Reporter) -> Session<'a> {
        Session {
            machine: &*self.machine,
            fetcher: &*self.fetcher,
            runner: &*self.runner,
            reporter,
            log: &self.log,
            work_dir: &self.work_dir,
        }
    }

    /// Deletes the downloads and unpacked installers; keeps the log and the
    /// installers' output.
    pub fn tidy(&self) {
        let Ok(entries) = fs::read_dir(&self.work_dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && let Err(error) = fs::remove_dir_all(entry.path())
            {
                self.log.line(format_args!(
                    "Could not delete {}: {error}",
                    entry.path().display()
                ));
            }
        }
    }
}
