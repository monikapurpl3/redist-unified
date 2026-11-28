//! The window's life: check right away, show what is missing with one
//! Install button, then the progress, then the result.
//!
//! The work runs on worker threads, which only update a shared [`UiModel`];
//! a timer copies that into the window, so the window never waits on a
//! download or an installer. `redist.exe` runs this with Windows itself, the
//! demo with stand-ins.

use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use redist_core::session::{Event, Options, Reporter};
use redist_core::windows::show_in_explorer;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, Timer, TimerMode, VecModel};

use crate::context::Context;
use crate::gui::{AppWindow, RowData, present};
use crate::ui::{TITLE, UiModel};

/// How often the window looks for news from the worker threads.
const REFRESH: Duration = Duration::from_millis(80);

type Shared = Arc<Mutex<UiModel>>;

fn lock(model: &Mutex<UiModel>) -> MutexGuard<'_, UiModel> {
    model.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Passes the session's events to the shared model.
struct ToModel(Shared);

impl Reporter for ToModel {
    fn report(&self, event: Event<'_>) {
        lock(&self.0).apply(event);
    }
}

/// Shows the window for `context`, or for why there is none, until it is
/// closed.
pub fn run(context: Result<Context, String>) -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;
    window.set_window_title(TITLE.into());
    let rows = Rc::new(VecModel::<RowData>::default());
    window.set_rows(ModelRc::from(rows.clone()));

    let (model, context): (Shared, Option<Arc<Context>>) = match context {
        Ok(context) => {
            let model = UiModel::new(&context.manifest, context.machine.native_arch());
            (Arc::new(Mutex::new(model)), Some(Arc::new(context)))
        }
        Err(message) => (
            Arc::new(Mutex::new(UiModel::failed_to_start(message))),
            None,
        ),
    };
    window.set_can_open_log(context.is_some());

    // Check right away.
    if let Some(context) = &context {
        let (context, model) = (context.clone(), model.clone());
        thread::spawn(move || {
            let reporter = ToModel(model.clone());
            let options = Options {
                only: None,
                check_only: true,
            };
            let result = context.session(&reporter).run(&context.manifest, &options);
            if let Err(error) = &result {
                context.log.line(format_args!("Stopped: {error}"));
            }
            lock(&model).checked(result.as_ref());
        });
    }

    // Install, once, when asked.
    window.on_install({
        let (context, model) = (context.clone(), model.clone());
        move || {
            let Some(context) = context.clone() else {
                return;
            };
            if !lock(&model).start_installing() {
                return;
            }
            let model = model.clone();
            thread::spawn(move || {
                let reporter = ToModel(model.clone());
                let result = context
                    .session(&reporter)
                    .run(&context.manifest, &Options::default());
                context.tidy();
                if let Err(error) = &result {
                    context.log.line(format_args!("Stopped: {error}"));
                }
                lock(&model).finished(result.as_ref());
            });
        }
    });

    window.on_restart({
        let (context, model) = (context.clone(), model.clone());
        move || {
            let Some(context) = &context else {
                return;
            };
            context.log.line("Restarting Windows");
            match (context.restart)() {
                Ok(()) => {
                    let _ = slint::quit_event_loop();
                }
                Err(error) => lock(&model).restart_failed(&error.to_string()),
            }
        }
    });

    window.on_open_log({
        let context = context.clone();
        move || {
            if let Some(context) = &context {
                let _ = show_in_explorer(&context.log_path);
            }
        }
    });

    window.on_dismiss(|| {
        let _ = slint::quit_event_loop();
    });

    // An installer must not be cut off halfway.
    window.window().on_close_requested({
        let model = model.clone();
        move || {
            if lock(&model).is_installing() {
                CloseRequestResponse::KeepWindowShown
            } else {
                CloseRequestResponse::HideWindow
            }
        }
    });

    // The worker threads only touch the model; this copies it into the
    // window when it changed.
    present(&window, &lock(&model), &rows);
    let timer = Timer::default();
    {
        let (window, model, rows) = (window.as_weak(), model.clone(), rows.clone());
        let mut shown = lock(&model).version();
        let mut has_icon = false;
        timer.start(TimerMode::Repeated, REFRESH, move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            if !has_icon {
                has_icon = use_exe_icon(window.window());
            }
            let model = lock(&model);
            if model.version() != shown {
                shown = model.version();
                present(&window, &model, &rows);
            }
        });
    }

    window.run()
}

/// Gives the window the exe's own icon (resource 1, `assets/redist.ico`,
/// embedded by build.rs): the small one in the title bar and the big one in
/// the taskbar, each at the size Windows wants for this scale. Slint's `icon`
/// property would make one 64 px picture serve both, and never use the
/// 16 px drawing.
///
/// Returns false until winit has made the window; an exe without the icon
/// keeps Windows' default.
fn use_exe_icon(window: &slint::Window) -> bool {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::dpi::PhysicalSize;
    use slint::winit_030::winit::platform::windows::{IconExtWindows, WindowExtWindows};
    use slint::winit_030::winit::window::Icon;

    let scale = window.scale_factor();
    let size = |logical: f32| {
        let side = (logical * scale).round() as u32;
        Some(PhysicalSize::new(side, side))
    };
    window
        .with_winit_window(|winit_window| {
            if let Ok(icon) = Icon::from_resource(1, size(16.0)) {
                winit_window.set_window_icon(Some(icon));
            }
            if let Ok(icon) = Icon::from_resource(1, size(32.0)) {
                winit_window.set_taskbar_icon(Some(icon));
            }
        })
        .is_some()
}
