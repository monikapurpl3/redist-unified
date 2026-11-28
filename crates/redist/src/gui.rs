//! The Slint window (`ui/app.slint`), and copying a [`UiModel`] into it.

slint::include_modules!();

use slint::{Model, VecModel};

use crate::ui::{self, UiModel};

impl From<ui::RowStatus> for RowStatus {
    fn from(status: ui::RowStatus) -> Self {
        match status {
            ui::RowStatus::Checking => RowStatus::Checking,
            ui::RowStatus::Present => RowStatus::Present,
            ui::RowStatus::Missing => RowStatus::Missing,
            ui::RowStatus::Downloading => RowStatus::Downloading,
            ui::RowStatus::Downloaded => RowStatus::Downloaded,
            ui::RowStatus::Installing => RowStatus::Installing,
            ui::RowStatus::Installed => RowStatus::Installed,
            ui::RowStatus::NeedsRestart => RowStatus::Restart,
            ui::RowStatus::Failed => RowStatus::Failed,
        }
    }
}

impl From<&ui::Phase> for Phase {
    fn from(phase: &ui::Phase) -> Self {
        match phase {
            ui::Phase::Checking => Phase::Checking,
            ui::Phase::Ready => Phase::Ready,
            ui::Phase::NothingToDo => Phase::Nothing,
            ui::Phase::Working => Phase::Working,
            ui::Phase::Done => Phase::Done,
            ui::Phase::NeedsRestart => Phase::Restart,
            ui::Phase::Failed(_) => Phase::Failed,
        }
    }
}

/// Shows `model` in `window`. `rows` is the model behind the window's list;
/// rows that did not change are left alone, so the list does not jump.
pub fn present(window: &AppWindow, model: &UiModel, rows: &VecModel<RowData>) {
    window.set_phase(model.phase().into());
    window.set_headline(model.headline().into());
    window.set_subline(model.subline().into());
    window.set_progress(model.progress());

    let fresh: Vec<RowData> = model
        .rows()
        .iter()
        .map(|row| RowData {
            name: row.name.as_str().into(),
            status: row.status.into(),
            detail: model.detail(row).into(),
            progress: row.download_fraction(),
        })
        .collect();
    if fresh.len() != rows.row_count() {
        rows.set_vec(fresh);
        return;
    }
    for (index, row) in fresh.into_iter().enumerate() {
        if rows.row_data(index).as_ref() != Some(&row) {
            rows.set_row_data(index, row);
        }
    }
}
