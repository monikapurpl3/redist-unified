//! Draws the window in each state, light and dark, into PNG files, without
//! opening a window or needing elevation:
//!
//!     cargo run -p redist --example screenshots -- <folder>
//!
//! The states come from `e2e/manifest.json` on an x64 machine.

use std::cell::RefCell;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use redist::gui::{AppWindow, RowData, present};
use redist::ui::UiModel;
use redist_core::detection::NativeArch;
use redist_core::manifest::Manifest;
use redist_core::session::{Event, Outcome, SessionError, Summary};
use slint::language::ColorScheme;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

const WIDTH: u32 = 520;
const HEIGHT: u32 = 620;

/// A platform whose windows draw into memory. It keeps the last one made,
/// to draw it.
#[derive(Default)]
struct Offscreen(RefCell<Option<Rc<MinimalSoftwareWindow>>>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
        *self.0.borrow_mut() = Some(window.clone());
        Ok(window)
    }
}

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "screenshots".into()),
    );
    std::fs::create_dir_all(&out).expect("the output folder can be made");
    let manifest = Manifest::parse(include_bytes!("../../../e2e/manifest.json")).unwrap();
    let platform = Rc::new(Offscreen::default());
    slint::platform::set_platform(Box::new(Shared(platform.clone()))).unwrap();

    for (name, model) in scenarios(&manifest) {
        for (scheme, suffix) in [(ColorScheme::Light, "light"), (ColorScheme::Dark, "dark")] {
            // A new window for every picture: Fluent buttons keep some colours
            // when the scheme switches in a running window, which the real
            // window never does (it follows Windows from the start).
            let ui = AppWindow::new().unwrap();
            let window = platform.0.borrow().clone().unwrap();
            let rows = Rc::new(VecModel::<RowData>::default());
            ui.set_rows(ModelRc::from(rows.clone()));
            ui.set_can_open_log(name != "8-could-not-start");
            ui.set_scheme_override(scheme);
            present(&ui, &model, &rows);
            ui.show().unwrap();
            // The list makes its rows while laying out, so the first frame
            // after a change can be incomplete: draw until it settles, as a
            // real window would.
            let mut pixels = vec![PremultipliedRgbaColor::default(); (WIDTH * HEIGHT) as usize];
            for _ in 0..3 {
                slint::platform::update_timers_and_animations();
                window.request_redraw();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, WIDTH as usize);
                });
            }
            let path = out.join(format!("{name}-{suffix}.png"));
            save(&path, &pixels);
            println!("{}", path.display());
            ui.hide().unwrap();
        }
    }
}

/// Lets the platform be shared with `main`, which draws its windows.
struct Shared(Rc<Offscreen>);

impl Platform for Shared {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        self.0.create_window_adapter()
    }
}

/// Every state the window can be in, in order.
fn scenarios(manifest: &Manifest) -> Vec<(&'static str, UiModel)> {
    let component = |id: &str| manifest.component(id).unwrap();
    let missing = ["vcredist-2013-x64", "vcredist-2013-x86", "directx-jun2010"];
    let summary = |restart_needed| Summary {
        results: Vec::new(),
        restart_needed,
    };

    let mut checking = UiModel::new(manifest, NativeArch::X64);
    for c in &manifest.components[..2] {
        checking.apply(Event::Checked {
            component: c,
            installed: true,
        });
    }

    let mut ready = UiModel::new(manifest, NativeArch::X64);
    for c in &manifest.components {
        ready.apply(Event::Checked {
            component: c,
            installed: !missing.contains(&c.id.as_str()),
        });
    }
    ready.checked(Ok(&summary(false)));

    let mut downloading = ready.clone();
    downloading.start_installing();
    for (id, received, total) in [
        ("vcredist-2013-x64", 7_200_744, 7_200_744),
        ("vcredist-2013-x86", 4_100_000, 6_510_136),
        ("directx-jun2010", 38_000_000, 100_275_120),
    ] {
        downloading.apply(Event::Progress {
            component: component(id),
            received,
            total,
        });
    }
    downloading.apply(Event::Downloaded {
        component: component("vcredist-2013-x64"),
    });

    let mut installing = downloading.clone();
    for id in missing {
        installing.apply(Event::Downloaded {
            component: component(id),
        });
    }
    installing.apply(Event::Done {
        component: component("vcredist-2013-x64"),
        outcome: Outcome::Installed,
    });
    installing.apply(Event::Installing {
        component: component("vcredist-2013-x86"),
    });

    let mut done = installing.clone();
    for id in &missing[1..] {
        done.apply(Event::Done {
            component: component(id),
            outcome: Outcome::Installed,
        });
    }
    let mut restart = done.clone();
    done.finished(Ok(&summary(false)));
    restart.apply(Event::Done {
        component: component("directx-jun2010"),
        outcome: Outcome::InstalledAfterRestart,
    });
    restart.finished(Ok(&summary(true)));

    let mut failed = installing.clone();
    failed.finished(Err(&SessionError::Failed {
        name: component("vcredist-2013-x86").name.clone(),
        code: 1603,
    }));

    let mut nothing = UiModel::new(manifest, NativeArch::X64);
    for c in &manifest.components {
        nothing.apply(Event::Checked {
            component: c,
            installed: true,
        });
    }
    nothing.checked(Ok(&summary(false)));

    let could_not_start = UiModel::failed_to_start(
        r"Could not read C:\Users\Someone\Downloads\manifest.json: The system cannot find the file specified. (os error 2).".into(),
    );

    vec![
        ("1-checking", checking),
        ("2-ready", ready),
        ("3-downloading", downloading),
        ("4-installing", installing),
        ("5-done", done),
        ("6-restart", restart),
        ("7-failed", failed),
        ("8-could-not-start", could_not_start),
        ("9-nothing-to-do", nothing),
    ]
}

/// Writes premultiplied pixels as an ordinary RGBA PNG.
fn save(path: &Path, pixels: &[PremultipliedRgbaColor]) {
    let file = BufWriter::new(File::create(path).expect("the PNG can be written"));
    let mut encoder = png::Encoder::new(file, WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    let data: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            if p.alpha == 0 {
                return [0, 0, 0, 0];
            }
            let straight = |c: u8| ((u16::from(c) * 255) / u16::from(p.alpha)) as u8;
            [
                straight(p.red),
                straight(p.green),
                straight(p.blue),
                p.alpha,
            ]
        })
        .collect();
    writer.write_image_data(&data).unwrap();
}
