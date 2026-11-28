//! What the two exes share, and the parts of them that have tests.
//!
//! - `redist.exe` is the window: [`app`] runs it, [`gui`] draws it from [`ui`].
//! - `redist-cli.exe` is the console version, for CI and scripts. Its
//!   command line and text are here.
//!
//! Both exes carry a `requireAdministrator` manifest, so test builds of the
//! exes themselves could not run without elevation; hence this library.

use std::path::{Path, PathBuf};

use redist_core::manifest::Manifest;
use redist_core::session::{BUSY_ATTEMPTS, BUSY_WAIT, Event, Outcome, Summary};

#[cfg(windows)]
pub mod app;
#[cfg(windows)]
pub mod context;
pub mod gui;
pub mod ui;

/// Where the window takes its manifest from: `--manifest <file>`, or else
/// `manifest.json` next to the exe. Both are unsigned, for development builds;
/// the signed manifest comes in milestone 4.
pub fn window_manifest(args: impl IntoIterator<Item = String>, exe: &Path) -> PathBuf {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--manifest"
            && let Some(file) = args.next()
        {
            return PathBuf::from(file);
        }
    }
    exe.with_file_name("manifest.json")
}

/// What `--help` prints.
pub const USAGE: &str = "\
Usage: redist-cli --manifest <file> [--only <component id>] [--check]

  --manifest <file>  the manifest to use (not signed: development builds only)
  --only <id>        install only this component
  --check            only show what is installed and what is missing";

/// The command line.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub manifest: PathBuf,
    pub only: Option<String>,
    pub check: bool,
}

/// Reads the command line, without the program name. `--help` is handled
/// before this.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let (mut manifest, mut only, mut check) = (None, None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--manifest" => {
                manifest = Some(PathBuf::from(args.next().ok_or("--manifest needs a file")?));
            }
            "--only" => only = Some(args.next().ok_or("--only needs a component id")?),
            "--check" => check = true,
            other => return Err(format!("Unknown argument {other:?}.")),
        }
    }
    Ok(Args {
        manifest: manifest.ok_or("--manifest is required for now.")?,
        only,
        check,
    })
}

/// The console line for an event, if it gets one.
pub fn event_text(event: Event<'_>) -> Option<String> {
    Some(match event {
        Event::Checked {
            component,
            installed,
        } => {
            let state = if installed { "installed" } else { "missing" };
            format!("  {state:<10} {}", component.name)
        }
        Event::Downloading { count } => {
            let files = if count == 1 { "file" } else { "files" };
            format!("\nDownloading {count} {files}...")
        }
        // Several downloads run at once; their progress would only jumble
        // the console.
        Event::Progress { .. } => return None,
        Event::Downloaded { component } => format!("  downloaded {}", component.name),
        Event::Installing { component } => format!("\nInstalling {}...", component.name),
        Event::Waiting { attempt, .. } => format!(
            "  Another installation is running. Trying again in {} seconds ({attempt} of {}).",
            BUSY_WAIT.as_secs(),
            BUSY_ATTEMPTS - 1
        ),
        Event::Done { outcome, .. } => match outcome {
            Outcome::Installed => "  done".to_owned(),
            Outcome::InstalledAfterRestart => {
                "  done; it finishes when Windows restarts".to_owned()
            }
            Outcome::InstallerFoundIt => "  it was already there".to_owned(),
            Outcome::NotForThisMachine | Outcome::Present | Outcome::Missing => return None,
        },
    })
}

/// The closing line: what is missing (when checking) or what was installed.
pub fn summary_text(manifest: &Manifest, summary: &Summary, check: bool) -> String {
    let names = |wanted: &[Outcome]| -> Vec<String> {
        summary
            .results
            .iter()
            .filter(|(_, outcome)| wanted.contains(outcome))
            .map(|(id, _)| {
                manifest
                    .component(id)
                    .map_or_else(|| id.clone(), |c| c.name.clone())
            })
            .collect()
    };
    if check {
        let missing = names(&[Outcome::Missing]);
        return if missing.is_empty() {
            "Nothing is missing.".to_owned()
        } else {
            format!("Missing: {}.", missing.join(", "))
        };
    }
    let installed = names(&[
        Outcome::Installed,
        Outcome::InstalledAfterRestart,
        Outcome::InstallerFoundIt,
    ]);
    if installed.is_empty() {
        "Everything was already installed.".to_owned()
    } else {
        format!("Installed: {}.", installed.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use redist_core::manifest::Manifest;
    use redist_core::session::{Event, Outcome, Summary};

    use super::{Args, event_text, parse_args, summary_text, window_manifest};

    #[test]
    fn the_window_finds_its_manifest() {
        let exe = std::path::Path::new(r"C:\test\redist.exe");
        let args = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            window_manifest(args(&[]), exe),
            PathBuf::from(r"C:\test\manifest.json")
        );
        assert_eq!(
            window_manifest(args(&["--manifest", r"D:\m.json"]), exe),
            PathBuf::from(r"D:\m.json")
        );
        // A dangling --manifest falls back to the default.
        assert_eq!(
            window_manifest(args(&["--manifest"]), exe),
            PathBuf::from(r"C:\test\manifest.json")
        );
    }

    /// Two components, enough for the wording.
    fn manifest() -> Manifest {
        let component = |id: &str, name: &str| {
            format!(
                r#"{{"id": "{id}", "name": "{name}", "arch": "any", "version": "1",
                    "installer": {{"type": "windows_feature", "feature": "F"}},
                    "detect": {{"type": "registry", "key": "HKLM\\SOFTWARE\\X", "view": "64"}},
                    "exit_ok": [0], "exit_reboot": [], "exit_already": [], "exit_retry": []}}"#
            )
        };
        let json = format!(
            r#"{{"schema": 1, "version": 1, "issued": "2026-11-24T00:00:00Z",
                "min_client": "0.1.0",
                "latest_client": {{"version": "0.1.0", "url": "https://download.example/"}},
                "components": [{}, {}]}}"#,
            component("a", "Runtime A"),
            component("b", "Runtime B")
        );
        Manifest::parse(json.as_bytes()).unwrap()
    }

    fn summary(outcomes: [Outcome; 2]) -> Summary {
        Summary {
            results: vec![("a".into(), outcomes[0]), ("b".into(), outcomes[1])],
            restart_needed: false,
        }
    }

    #[test]
    fn progress_lines() {
        let manifest = manifest();
        let a = &manifest.components[0];
        let line = |event| event_text(event);
        assert_eq!(
            line(Event::Checked {
                component: a,
                installed: false
            })
            .unwrap(),
            "  missing    Runtime A"
        );
        assert_eq!(
            line(Event::Downloading { count: 1 }).unwrap(),
            "\nDownloading 1 file..."
        );
        assert_eq!(
            line(Event::Waiting {
                component: a,
                attempt: 2
            })
            .unwrap(),
            "  Another installation is running. Trying again in 30 seconds (2 of 9)."
        );
        assert_eq!(
            line(Event::Done {
                component: a,
                outcome: Outcome::InstalledAfterRestart
            })
            .unwrap(),
            "  done; it finishes when Windows restarts"
        );
        assert_eq!(
            line(Event::Progress {
                component: a,
                received: 1,
                total: 1
            }),
            None
        );
    }

    #[test]
    fn closing_lines() {
        let manifest = manifest();
        let text = |outcomes, check| summary_text(&manifest, &summary(outcomes), check);
        assert_eq!(
            text([Outcome::Missing, Outcome::Present], true),
            "Missing: Runtime A."
        );
        assert_eq!(
            text([Outcome::Present, Outcome::Present], true),
            "Nothing is missing."
        );
        assert_eq!(
            text([Outcome::Installed, Outcome::InstallerFoundIt], false),
            "Installed: Runtime A, Runtime B."
        );
        assert_eq!(
            text([Outcome::Present, Outcome::NotForThisMachine], false),
            "Everything was already installed."
        );
    }

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn arguments() {
        assert_eq!(
            parse(&["--manifest", "m.json"]).unwrap(),
            Args {
                manifest: PathBuf::from("m.json"),
                only: None,
                check: false
            }
        );
        assert_eq!(
            parse(&["--check", "--only", "netfx35", "--manifest", "m.json"]).unwrap(),
            Args {
                manifest: PathBuf::from("m.json"),
                only: Some("netfx35".into()),
                check: true
            }
        );
        assert!(parse(&[]).unwrap_err().contains("--manifest is required"));
        assert!(parse(&["--manifest"]).unwrap_err().contains("needs a file"));
        assert!(
            parse(&["--manifest", "m.json", "--only"])
                .unwrap_err()
                .contains("needs a component id")
        );
        assert!(parse(&["--force"]).unwrap_err().contains("\"--force\""));
    }
}
