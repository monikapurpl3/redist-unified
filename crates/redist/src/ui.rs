//! What the window shows, as plain data: one row per runtime that belongs on
//! this machine, the phase of the run, and the words and progress for it.
//!
//! The window (`ui/app.slint`, filled in by `gui.rs`) only draws this, so the
//! tests are here.

use redist_core::detection::{NativeArch, applies_to};
use redist_core::manifest::Manifest;
use redist_core::session::{Event, Outcome, SessionError, Summary};

/// The window's title. The product name is still open, so this is a working
/// name.
pub const TITLE: &str = "Runtime Installer";

/// Where a runtime stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStatus {
    Checking,
    Present,
    Missing,
    Downloading,
    Downloaded,
    Installing,
    Installed,
    NeedsRestart,
    Failed,
}

/// One runtime in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub status: RowStatus,
    /// Bytes received and expected, while downloading.
    pub received: u64,
    pub total: u64,
    /// Another installation is running, and this one waits for it.
    pub waiting: bool,
}

impl Row {
    /// How far its download is, from 0 to 1.
    pub fn download_fraction(&self) -> f32 {
        match self.status {
            RowStatus::Downloading if self.total > 0 => {
                (self.received as f64 / self.total as f64).clamp(0.0, 1.0) as f32
            }
            RowStatus::Downloading => 0.0,
            _ => 1.0,
        }
    }

    /// How far the whole job for it is, from 0 to 1: downloading is the
    /// first 60 %, installing the rest.
    fn job_fraction(&self) -> f32 {
        match self.status {
            RowStatus::Checking | RowStatus::Missing | RowStatus::Failed => 0.0,
            RowStatus::Downloading => 0.6 * self.download_fraction(),
            RowStatus::Downloaded => 0.6,
            RowStatus::Installing => 0.75,
            RowStatus::Present | RowStatus::Installed | RowStatus::NeedsRestart => 1.0,
        }
    }
}

/// Where the whole run stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Detecting.
    Checking,
    /// Something is missing; the Install button waits.
    Ready,
    /// Everything is there already.
    NothingToDo,
    /// Downloading and installing.
    Working,
    /// Everything installed.
    Done,
    /// Everything installed, and Windows has to restart.
    NeedsRestart,
    /// It stopped, for this reason.
    Failed(String),
}

/// What the window shows.
#[derive(Clone, Debug)]
pub struct UiModel {
    rows: Vec<Row>,
    phase: Phase,
    /// Indexes of the rows being installed.
    installing: Vec<usize>,
    /// Goes up with every change, so the window redraws only when needed.
    version: u64,
}

impl UiModel {
    /// The rows for every component that belongs on a `native` machine, all
    /// being checked.
    pub fn new(manifest: &Manifest, native: NativeArch) -> UiModel {
        let rows = manifest
            .components
            .iter()
            .filter(|c| applies_to(c.arch, native))
            .map(|c| Row {
                id: c.id.clone(),
                name: c.name.clone(),
                status: RowStatus::Checking,
                received: 0,
                total: 0,
                waiting: false,
            })
            .collect();
        UiModel {
            rows,
            phase: Phase::Checking,
            installing: Vec::new(),
            version: 0,
        }
    }

    /// A window that can only say why the run could not start.
    pub fn failed_to_start(message: String) -> UiModel {
        UiModel {
            rows: Vec::new(),
            phase: Phase::Failed(message),
            installing: Vec::new(),
            version: 0,
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Whether closing the window now would interrupt an installation.
    pub fn is_installing(&self) -> bool {
        self.phase == Phase::Working
    }

    /// Takes in what the session reports.
    pub fn apply(&mut self, event: Event<'_>) {
        let component = match event {
            Event::Checked { component, .. }
            | Event::Progress { component, .. }
            | Event::Downloaded { component }
            | Event::Installing { component }
            | Event::Waiting { component, .. }
            | Event::Done { component, .. } => component,
            Event::Downloading { .. } => return,
        };
        let Some(row) = self.rows.iter_mut().find(|row| row.id == component.id) else {
            return;
        };
        match event {
            Event::Checked { installed, .. } => {
                row.status = if installed {
                    RowStatus::Present
                } else {
                    RowStatus::Missing
                };
            }
            Event::Progress {
                received, total, ..
            } => {
                row.status = RowStatus::Downloading;
                row.received = received;
                row.total = total;
            }
            Event::Downloaded { .. } => row.status = RowStatus::Downloaded,
            Event::Installing { .. } => {
                row.status = RowStatus::Installing;
                row.waiting = false;
            }
            Event::Waiting { .. } => {
                row.status = RowStatus::Installing;
                row.waiting = true;
            }
            Event::Done { outcome, .. } => {
                row.waiting = false;
                row.status = match outcome {
                    Outcome::Installed | Outcome::InstallerFoundIt => RowStatus::Installed,
                    Outcome::InstalledAfterRestart => RowStatus::NeedsRestart,
                    Outcome::Present => RowStatus::Present,
                    Outcome::Missing | Outcome::NotForThisMachine => row.status,
                };
            }
            Event::Downloading { .. } => {}
        }
        self.version += 1;
    }

    /// The check finished.
    pub fn checked(&mut self, result: Result<&Summary, &SessionError>) {
        match result {
            Ok(_) if self.missing() > 0 => self.phase = Phase::Ready,
            Ok(_) => self.phase = Phase::NothingToDo,
            Err(error) => self.fail(error),
        }
        self.version += 1;
    }

    /// The Install button was pressed. Returns whether installing may start,
    /// which it may only once, and only when something is missing.
    pub fn start_installing(&mut self) -> bool {
        if self.phase != Phase::Ready {
            return false;
        }
        self.installing = (0..self.rows.len())
            .filter(|&i| self.rows[i].status == RowStatus::Missing)
            .collect();
        self.phase = Phase::Working;
        self.version += 1;
        true
    }

    /// The installation finished.
    pub fn finished(&mut self, result: Result<&Summary, &SessionError>) {
        match result {
            Ok(summary) if summary.restart_needed => self.phase = Phase::NeedsRestart,
            Ok(_) => self.phase = Phase::Done,
            Err(error) => self.fail(error),
        }
        self.version += 1;
    }

    /// Restarting Windows did not work.
    pub fn restart_failed(&mut self, reason: &str) {
        self.phase = Phase::Failed(format!(
            "Windows could not be restarted ({reason}). Restart it yourself to finish."
        ));
        self.version += 1;
    }

    fn fail(&mut self, error: &SessionError) {
        // The run stopped: what was still on its way will not come.
        for &i in &self.installing {
            let row = &mut self.rows[i];
            if matches!(
                row.status,
                RowStatus::Downloading | RowStatus::Downloaded | RowStatus::Installing
            ) {
                row.status = RowStatus::Missing;
                row.waiting = false;
            }
        }
        if let Some(name) = error.component_name() {
            for row in self.rows.iter_mut().filter(|row| row.name == name) {
                row.status = RowStatus::Failed;
                row.waiting = false;
            }
        }
        self.phase = Phase::Failed(error.to_string());
    }

    fn missing(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.status == RowStatus::Missing)
            .count()
    }

    /// What a row says under its name.
    pub fn detail(&self, row: &Row) -> String {
        match row.status {
            RowStatus::Checking => "Checking…".into(),
            RowStatus::Present => "Already installed".into(),
            RowStatus::Missing if self.phase == Phase::Working => "Waiting".into(),
            RowStatus::Missing => "Not installed".into(),
            RowStatus::Downloading => {
                format!(
                    "Downloading · {} %",
                    (row.download_fraction() * 100.0).round()
                )
            }
            RowStatus::Downloaded => "Downloaded, waiting to install".into(),
            RowStatus::Installing if row.waiting => {
                "Waiting for another installation to finish".into()
            }
            RowStatus::Installing => "Installing…".into(),
            RowStatus::Installed => "Installed".into(),
            RowStatus::NeedsRestart => "Installed, finishes after a restart".into(),
            RowStatus::Failed => "Did not install".into(),
        }
    }

    /// The big line at the top.
    pub fn headline(&self) -> String {
        match &self.phase {
            Phase::Checking => "Checking your PC".into(),
            Phase::Ready => match self.missing() {
                1 => "1 runtime is missing".into(),
                n => format!("{n} runtimes are missing"),
            },
            Phase::NothingToDo => "You're all set".into(),
            Phase::Working => "Installing".into(),
            Phase::Done => "All done".into(),
            Phase::NeedsRestart => "Almost done".into(),
            Phase::Failed(_) => "Something went wrong".into(),
        }
    }

    /// The sentence under it.
    pub fn subline(&self) -> String {
        match &self.phase {
            Phase::Checking => "Looking for the runtimes that programs and games need.".into(),
            Phase::Ready => {
                "Install them so the programs that need them start without errors.".into()
            }
            Phase::NothingToDo => {
                "Everything programs need from this list is already installed.".into()
            }
            Phase::Working => {
                let done = self
                    .installing
                    .iter()
                    .filter(|&&i| {
                        matches!(
                            self.rows[i].status,
                            RowStatus::Installed | RowStatus::NeedsRestart | RowStatus::Present
                        )
                    })
                    .count();
                format!(
                    "{done} of {} ready. You can keep using your PC meanwhile.",
                    self.installing.len()
                )
            }
            Phase::Done => "Everything is installed.".into(),
            Phase::NeedsRestart => "Restart Windows to finish installing.".into(),
            Phase::Failed(message) => message.clone(),
        }
    }

    /// The progress bar, from 0 to 1, while installing.
    pub fn progress(&self) -> f32 {
        if self.installing.is_empty() {
            return 0.0;
        }
        let sum: f32 = self
            .installing
            .iter()
            .map(|&i| self.rows[i].job_fraction())
            .sum();
        sum / self.installing.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use redist_core::detection::NativeArch;
    use redist_core::manifest::{Component, Manifest};
    use redist_core::session::{Event, Outcome, SessionError, Summary};

    use super::{Phase, RowStatus, UiModel};

    /// The real CI manifest: VC++ v14 and 2013, DirectX, .NET 11, with ARM64
    /// variants.
    fn manifest() -> Manifest {
        Manifest::parse(include_bytes!("../../../e2e/manifest.json")).unwrap()
    }

    fn component<'m>(manifest: &'m Manifest, id: &str) -> &'m Component {
        manifest.component(id).unwrap()
    }

    fn summary(restart_needed: bool) -> Summary {
        Summary {
            results: Vec::new(),
            restart_needed,
        }
    }

    /// A model after checking, with the two VC++ 2013 runtimes missing.
    fn checked(manifest: &Manifest) -> UiModel {
        let mut model = UiModel::new(manifest, NativeArch::X64);
        for c in &manifest.components {
            model.apply(Event::Checked {
                component: c,
                installed: !c.id.starts_with("vcredist-2013"),
            });
        }
        model.checked(Ok(&summary(false)));
        model
    }

    fn status(model: &UiModel, id: &str) -> RowStatus {
        model.rows().iter().find(|r| r.id == id).unwrap().status
    }

    #[test]
    fn rows_are_only_what_belongs_on_this_machine() {
        let manifest = manifest();
        let x64 = UiModel::new(&manifest, NativeArch::X64);
        assert_eq!(x64.rows().len(), 7);
        assert!(x64.rows().iter().all(|r| !r.id.ends_with("arm64")));
        assert!(x64.rows().iter().all(|r| r.status == RowStatus::Checking));
        assert_eq!(UiModel::new(&manifest, NativeArch::Arm64).rows().len(), 9);
        assert_eq!(x64.headline(), "Checking your PC");
    }

    #[test]
    fn after_checking_the_missing_ones_wait_for_the_button() {
        let manifest = manifest();
        let model = checked(&manifest);
        assert_eq!(model.phase(), &Phase::Ready);
        assert_eq!(model.headline(), "2 runtimes are missing");
        assert_eq!(status(&model, "vcredist-2013-x64"), RowStatus::Missing);
        assert_eq!(status(&model, "directx-jun2010"), RowStatus::Present);
        let row = &model.rows()[0];
        assert_eq!(model.detail(row), "Already installed");

        let mut everything = UiModel::new(&manifest, NativeArch::X64);
        for c in &manifest.components {
            everything.apply(Event::Checked {
                component: c,
                installed: true,
            });
        }
        everything.checked(Ok(&summary(false)));
        assert_eq!(everything.phase(), &Phase::NothingToDo);
        assert_eq!(everything.headline(), "You're all set");
    }

    #[test]
    fn installing_starts_once_and_shows_progress() {
        let manifest = manifest();
        let mut model = checked(&manifest);
        assert!(model.start_installing());
        assert!(!model.start_installing(), "a second press started again");
        assert!(model.is_installing());
        assert_eq!(model.progress(), 0.0);
        assert_eq!(
            model.subline(),
            "0 of 2 ready. You can keep using your PC meanwhile."
        );

        let vc64 = component(&manifest, "vcredist-2013-x64");
        let vc86 = component(&manifest, "vcredist-2013-x86");
        model.apply(Event::Progress {
            component: vc64,
            received: 50,
            total: 100,
        });
        let row = model.rows().iter().find(|r| r.id == vc64.id).unwrap();
        assert_eq!(model.detail(row), "Downloading · 50 %");
        // The other one is still queued.
        let queued = model.rows().iter().find(|r| r.id == vc86.id).unwrap();
        assert_eq!(model.detail(queued), "Waiting");
        assert!(
            (model.progress() - 0.15).abs() < 1e-6,
            "{}",
            model.progress()
        );

        model.apply(Event::Downloaded { component: vc64 });
        model.apply(Event::Installing { component: vc64 });
        model.apply(Event::Waiting {
            component: vc64,
            attempt: 1,
        });
        let row = model.rows().iter().find(|r| r.id == vc64.id).unwrap();
        assert_eq!(
            model.detail(row),
            "Waiting for another installation to finish"
        );
        model.apply(Event::Done {
            component: vc64,
            outcome: Outcome::Installed,
        });
        assert_eq!(
            model.subline(),
            "1 of 2 ready. You can keep using your PC meanwhile."
        );
        assert!(
            (model.progress() - 0.5).abs() < 1e-6,
            "{}",
            model.progress()
        );

        model.apply(Event::Done {
            component: vc86,
            outcome: Outcome::InstalledAfterRestart,
        });
        model.finished(Ok(&summary(true)));
        assert_eq!(model.phase(), &Phase::NeedsRestart);
        assert_eq!(model.headline(), "Almost done");
        assert_eq!(status(&model, "vcredist-2013-x86"), RowStatus::NeedsRestart);
        assert!(!model.is_installing());
        assert!((model.progress() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_failure_marks_its_runtime_and_says_why() {
        let manifest = manifest();
        let mut model = checked(&manifest);
        model.start_installing();
        let (vc64, vc86) = (
            component(&manifest, "vcredist-2013-x64"),
            component(&manifest, "vcredist-2013-x86"),
        );
        model.apply(Event::Downloaded { component: vc64 });
        model.apply(Event::Downloaded { component: vc86 });
        model.apply(Event::Installing { component: vc86 });
        let error = SessionError::Failed {
            name: vc86.name.clone(),
            code: 1603,
        };
        model.finished(Err(&error));
        assert_eq!(status(&model, "vcredist-2013-x86"), RowStatus::Failed);
        // Downloaded, but the run stopped before installing it.
        assert_eq!(status(&model, "vcredist-2013-x64"), RowStatus::Missing);
        let row = model.rows().iter().find(|r| r.id == vc64.id).unwrap();
        assert_eq!(model.detail(row), "Not installed");
        assert_eq!(model.headline(), "Something went wrong");
        assert!(
            model.subline().contains("exit code 1603"),
            "{}",
            model.subline()
        );
        assert!(!model.is_installing());

        let model = UiModel::failed_to_start("No manifest.".into());
        assert!(model.rows().is_empty());
        assert_eq!(model.subline(), "No manifest.");
    }

    #[test]
    fn every_change_is_counted() {
        let manifest = manifest();
        let mut model = UiModel::new(&manifest, NativeArch::X64);
        let before = model.version();
        model.apply(Event::Downloading { count: 1 });
        assert_eq!(
            model.version(),
            before,
            "nothing changed, nothing to redraw"
        );
        model.apply(Event::Checked {
            component: &manifest.components[0],
            installed: true,
        });
        assert!(model.version() > before);
    }
}
