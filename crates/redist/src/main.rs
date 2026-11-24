//! `redist.exe`: installs the Windows runtimes programs commonly ask for.
//!
//! For now it reads the manifest from a local file, which is not signed:
//! development builds only. Fetching the signed manifest comes in
//! milestone 4.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Command;

use redist::{Args, USAGE, event_text, parse_args, summary_text};
use redist_core::log::Log;
use redist_core::manifest::{KnownFolder, Manifest};
use redist_core::session::{Event, Options, Reporter, Session};
use redist_core::windows::{
    ProcessRunner, WinHttpFetcher, WindowsMachine, console_is_ours, create_work_dir,
    known_folder_path,
};

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit codes.
const OK: i32 = 0;
const FAILED: i32 = 1;
const USAGE_ERROR: i32 = 2;
/// Everything installed, and Windows still has to restart.
const RESTART_NEEDED: i32 = 3010;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = if args.iter().any(|a| a == "--help" || a == "/?") {
        println!("{USAGE}");
        OK
    } else {
        match parse_args(args) {
            Ok(args) => run(&args),
            Err(message) => {
                eprintln!("{message}\n\n{USAGE}");
                USAGE_ERROR
            }
        }
    };
    // Started by double-clicking, the window would close before anyone
    // could read it.
    if console_is_ours() {
        print!("\nPress Enter to close.");
        let _ = io::stdout().flush();
        let _ = io::stdin().lock().read_line(&mut String::new());
    }
    std::process::exit(code);
}

fn run(args: &Args) -> i32 {
    println!("{NAME} {VERSION}, development build: the manifest is not signed.\n");
    let manifest = match fs::read(&args.manifest) {
        Ok(bytes) => match Manifest::parse(&bytes) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("{}: {error}", args.manifest.display());
                return USAGE_ERROR;
            }
        },
        Err(error) => {
            eprintln!("Could not read {}: {error}", args.manifest.display());
            return USAGE_ERROR;
        }
    };

    let prepared = (|| {
        let machine = WindowsMachine::new()?;
        let fetcher = WinHttpFetcher::new()?;
        let work_dir = create_work_dir()?;
        let log_path = work_dir.join(format!("{NAME}.log"));
        let log = Log::create(&log_path)?;
        io::Result::Ok((machine, fetcher, work_dir, log_path, log))
    })();
    let (machine, fetcher, work_dir, log_path, log) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("Could not get started: {error}.");
            return FAILED;
        }
    };
    log.line(format_args!(
        "{NAME} {VERSION}; manifest {}; {args:?}",
        args.manifest.display()
    ));

    println!("Checking what is installed...");
    let session = Session {
        machine: &machine,
        fetcher: &fetcher,
        runner: &ProcessRunner,
        reporter: &Console,
        log: &log,
        work_dir: &work_dir,
    };
    let options = Options {
        only: args.only.as_deref(),
        check_only: args.check,
    };
    let result = session.run(&manifest, &options);
    tidy(&work_dir, &log);

    match result {
        Ok(summary) => {
            println!("\n{}", summary_text(&manifest, &summary, args.check));
            println!("The log is {}", log_path.display());
            if summary.restart_needed {
                offer_restart(&log)
            } else {
                OK
            }
        }
        Err(error) => {
            log.line(format_args!("Stopped: {error}"));
            eprintln!("\n{error}\nThe details are in {}", log_path.display());
            FAILED
        }
    }
}

/// Shows progress in the console.
struct Console;

impl Reporter for Console {
    fn report(&self, event: Event<'_>) {
        if let Some(text) = event_text(event) {
            println!("{text}");
        }
    }
}

/// Asks once whether to restart now. Returns the exit code.
fn offer_restart(log: &Log) -> i32 {
    print!("\nWindows needs to restart to finish. Restart now? [y/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    let _ = io::stdin().lock().read_line(&mut answer);
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Restart Windows later to finish.");
        return RESTART_NEEDED;
    }
    log.line("Restarting Windows");
    let restarted = known_folder_path(KnownFolder::System32).and_then(|system32| {
        Command::new(system32.join("shutdown.exe"))
            .args(["/r", "/t", "0"])
            .status()
    });
    match restarted {
        Ok(status) if status.success() => OK,
        Ok(status) => {
            eprintln!("Could not restart Windows ({status}). Restart it yourself to finish.");
            RESTART_NEEDED
        }
        Err(error) => {
            eprintln!("Could not restart Windows: {error}. Restart it yourself to finish.");
            RESTART_NEEDED
        }
    }
}

/// Deletes the downloads and unpacked installers; keeps the log and the
/// installers' output.
fn tidy(work_dir: &Path, log: &Log) {
    let Ok(entries) = fs::read_dir(work_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir())
            && let Err(error) = fs::remove_dir_all(entry.path())
        {
            log.line(format_args!(
                "Could not delete {}: {error}",
                entry.path().display()
            ));
        }
    }
}
