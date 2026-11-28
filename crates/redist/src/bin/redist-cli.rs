//! `redist-cli.exe`: the console version of the installer, for CI and
//! scripts. Users double-click `redist.exe`, the window.
//!
//! For now it reads the manifest from a local file, which is not signed:
//! development builds only. Fetching the signed manifest comes in
//! milestone 4.

use std::io::{self, BufRead, Write};

use redist::context::Context;
use redist::{Args, USAGE, event_text, parse_args, summary_text};
use redist_core::log::Log;
use redist_core::session::{Event, Options, Reporter};
use redist_core::windows::{console_is_ours, restart_windows};

const NAME: &str = "redist-cli";
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
    let manifest = match Context::read_manifest(&args.manifest) {
        Ok(manifest) => manifest,
        Err(message) => {
            eprintln!("{message}");
            return USAGE_ERROR;
        }
    };
    let context = match Context::prepare(manifest, NAME, VERSION) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("Could not get started: {error}.");
            return FAILED;
        }
    };
    context.log.line(format_args!("{args:?}"));

    println!("Checking what is installed...");
    let options = Options {
        only: args.only.as_deref(),
        check_only: args.check,
    };
    let result = context.session(&Console).run(&context.manifest, &options);
    context.tidy();

    match result {
        Ok(summary) => {
            println!(
                "\n{}",
                summary_text(&context.manifest, &summary, args.check)
            );
            println!("The log is {}", context.log_path.display());
            if summary.restart_needed {
                offer_restart(&context.log)
            } else {
                OK
            }
        }
        Err(error) => {
            context.log.line(format_args!("Stopped: {error}"));
            eprintln!(
                "\n{error}\nThe details are in {}",
                context.log_path.display()
            );
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
    match restart_windows() {
        Ok(()) => OK,
        Err(error) => {
            eprintln!("Could not restart Windows: {error}. Restart it yourself to finish.");
            RESTART_NEEDED
        }
    }
}
