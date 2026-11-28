//! Running installers, the console the client runs in, and the few things it
//! asks of Windows at the end: a restart, or showing the log.

use std::fs::File;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::System::Console::GetConsoleProcessList;

use super::{known_folder_path, run_hidden};
use crate::manifest::KnownFolder;
use crate::session::Runner;

/// How long an installer may run before it is stopped. .NET Framework 3.5
/// can take many minutes when Windows Update supplies it.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Runs installers without a window, from the work folder. Each one's
/// console output (DISM writes some) goes to `<program>.output.txt` there.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, program: &Path, args: &[String], dir: &Path) -> io::Result<u32> {
        let name = program
            .file_stem()
            .map_or_else(|| "installer".into(), |stem| stem.to_string_lossy());
        let output = File::create(dir.join(format!("{name}.output.txt")))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output);
        run_hidden(command, INSTALL_TIMEOUT)
    }

    fn wait(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// Whether this process is the only one on its console: it was started by
/// double-clicking, and the window closes as soon as it exits.
pub fn console_is_ours() -> bool {
    let mut ids = [0u32; 2];
    // SAFETY: ids has room for the two process ids asked for.
    unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) == 1 }
}

/// Restarts Windows now, through System32's `shutdown.exe`.
pub fn restart_windows() -> io::Result<()> {
    let shutdown = known_folder_path(KnownFolder::System32)?.join("shutdown.exe");
    let status = Command::new(shutdown).args(["/r", "/t", "0"]).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "shutdown.exe ended with {status}"
        )))
    }
}

/// Opens an Explorer window with `file` selected.
pub fn show_in_explorer(file: &Path) -> io::Result<()> {
    let explorer = known_folder_path(KnownFolder::Windows)?.join("explorer.exe");
    // Explorer reads `/select,"<path>"` itself, so it gets the argument as
    // written instead of std's quoting.
    Command::new(explorer)
        .raw_arg(format!("/select,\"{}\"", file.display()))
        .spawn()
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::ProcessRunner;
    use crate::manifest::KnownFolder;
    use crate::session::Runner;
    use crate::windows::{known_folder_path, run_hidden};

    fn cmd() -> PathBuf {
        known_folder_path(KnownFolder::System32)
            .unwrap()
            .join("cmd.exe")
    }

    #[test]
    fn installers_run_in_their_folder_with_their_output_kept() {
        let dir = std::env::temp_dir().join(format!("redist-runner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let code = ProcessRunner
            .run(&cmd(), &["/c".into(), "cd & exit 7".into()], &dir)
            .unwrap();
        assert_eq!(code, 7);
        let output = std::fs::read_to_string(dir.join("cmd.output.txt")).unwrap();
        assert_eq!(
            output.trim().to_lowercase(),
            dir.display().to_string().to_lowercase()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_program_that_runs_too_long_is_stopped() {
        let mut command = std::process::Command::new(cmd());
        command.args(["/c", "ping -n 4 127.0.0.1 >nul"]);
        let started = std::time::Instant::now();
        let error = run_hidden(command, Duration::from_millis(300)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "it was not stopped"
        );
    }
}
