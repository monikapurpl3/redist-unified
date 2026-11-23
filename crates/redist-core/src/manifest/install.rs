//! How a component is installed, and what its installer's exit codes mean.

use std::collections::HashSet;

use serde::Deserialize;

use super::paths::RelativePath;

/// How to run a component's installer. Installers always run silently, one
/// at a time.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Installer {
    /// Run the downloaded exe with `args`.
    Exe { args: Vec<String> },
    /// Run `msiexec /i <downloaded file>`, followed by `args`.
    Msi { args: Vec<String> },
    /// A self-extracting package such as the DirectX June 2010 runtime. Run
    /// it with `extract_args`, where `{dir}` stands for a new empty folder,
    /// then run `run` from that folder with `args`.
    Extract {
        extract_args: Vec<String>,
        run: RelativePath,
        args: Vec<String>,
    },
    /// Turn on an optional Windows feature with DISM, such as `NetFx3`.
    /// Nothing is downloaded.
    WindowsFeature { feature: String },
}

impl Installer {
    /// Whether this installer runs a downloaded file.
    pub fn downloads(&self) -> bool {
        !matches!(self, Installer::WindowsFeature { .. })
    }

    /// Checks what the field types cannot.
    pub(super) fn check(&self) -> Result<(), String> {
        match self {
            Installer::Extract { extract_args, .. }
                if !extract_args.iter().any(|arg| arg.contains("{dir}")) =>
            {
                Err("extract_args must contain {dir}, the folder to extract into".into())
            }
            Installer::WindowsFeature { feature }
                if feature.is_empty()
                    || !feature
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) =>
            {
                Err(format!(
                    "feature name {feature:?} must be letters, digits, -, _ and ."
                ))
            }
            _ => Ok(()),
        }
    }
}

/// What an installer's exit code means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitOutcome {
    /// Installed.
    Installed,
    /// Installed, and Windows needs a restart. The client asks once, at the
    /// end.
    RebootNeeded,
    /// This or a newer version was already installed. Not an error.
    AlreadyInstalled,
    /// Another installation is running. Wait and try again.
    RetryLater,
    /// Anything else: stop and report it.
    Failed,
}

/// A component's exit codes by meaning, from its `exit_ok`, `exit_reboot`,
/// `exit_already` and `exit_retry`. No code appears twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitCodes {
    ok: Vec<u32>,
    reboot: Vec<u32>,
    already: Vec<u32>,
    retry: Vec<u32>,
}

impl ExitCodes {
    pub(super) fn new(
        ok: Vec<u32>,
        reboot: Vec<u32>,
        already: Vec<u32>,
        retry: Vec<u32>,
    ) -> Result<Self, String> {
        if ok.is_empty() {
            return Err("exit_ok needs at least one code".into());
        }
        let mut seen = HashSet::new();
        for code in ok.iter().chain(&reboot).chain(&already).chain(&retry) {
            if !seen.insert(*code) {
                return Err(format!("exit code {code} is listed more than once"));
            }
        }
        Ok(ExitCodes {
            ok,
            reboot,
            already,
            retry,
        })
    }

    /// What `code` means for this component.
    pub fn classify(&self, code: u32) -> ExitOutcome {
        if self.ok.contains(&code) {
            ExitOutcome::Installed
        } else if self.reboot.contains(&code) {
            ExitOutcome::RebootNeeded
        } else if self.already.contains(&code) {
            ExitOutcome::AlreadyInstalled
        } else if self.retry.contains(&code) {
            ExitOutcome::RetryLater
        } else {
            ExitOutcome::Failed
        }
    }
}
