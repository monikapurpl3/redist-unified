//! A safe demo of the window: the real window and the real install session,
//! on a pretend machine. Nothing is downloaded, installed or restarted, no
//! registry or system file is touched, and it needs no elevation. Its log and
//! its pretend downloads go to the temp folder.
//!
//!     cargo run -p redist --example demo               # ends with "Restart now"
//!     cargo run -p redist --example demo -- --fail     # an installer fails
//!     cargo run -p redist --example demo -- --busy     # waits for "another installation"
//!     cargo run -p redist --example demo -- --nothing  # everything is already there
//!
//! Add `--headless` to run the same in the console, without a window (CI
//! does this, to keep the demo working).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use redist::context::Context;
use redist::{event_text, summary_text};
use redist_core::detection::{Machine, NativeArch, RegistryValue};
use redist_core::manifest::{HttpsUrl, KnownFolder, Manifest, RegistryView};
use redist_core::session::{Event, Fetcher, Options, Reporter, Runner};
use redist_core::version::Version;
use sha2::Digest;

/// How fast one pretend connection downloads.
const BYTES_PER_SECOND: u64 = 1_500_000;

/// How long a pretend installer runs.
const INSTALL_TIME: Duration = Duration::from_millis(2500);

/// One pretend runtime.
struct Item {
    id: &'static str,
    name: &'static str,
    megabytes: u64,
    present: bool,
    /// The exit codes its installer gives, one per run; the last repeats.
    exit_codes: &'static [u32],
}

fn items(scenario: &str) -> Vec<Item> {
    let item = |id, name, megabytes, present, exit_codes| Item {
        id,
        name,
        megabytes,
        present: present || scenario == "--nothing",
        exit_codes,
    };
    vec![
        item(
            "vc14-x64",
            "Visual C++ v14 Redistributable (x64)",
            18,
            true,
            &[0],
        ),
        item(
            "vc14-x86",
            "Visual C++ v14 Redistributable (x86)",
            7,
            true,
            &[0],
        ),
        item(
            "vc2013-x64",
            "Visual C++ 2013 Redistributable (x64)",
            6,
            false,
            &[0],
        ),
        item(
            "vc2013-x86",
            "Visual C++ 2013 Redistributable (x86)",
            4,
            false,
            if scenario == "--busy" {
                &[1618, 1618, 0]
            } else {
                &[0]
            },
        ),
        item(
            "directx",
            "DirectX End-User Runtime (June 2010)",
            24,
            false,
            if scenario == "--fail" { &[1603] } else { &[0] },
        ),
        item(
            "dotnet11-x64",
            ".NET 11 Desktop Runtime (x64)",
            30,
            false,
            &[3010],
        ),
        item(
            "dotnet11-x86",
            ".NET 11 Desktop Runtime (x86)",
            28,
            true,
            &[0],
        ),
    ]
}

/// The pretend file's byte at `offset`: always the same, so its hash is
/// known in advance.
fn byte_at(id: &str, offset: u64) -> u8 {
    let seed = id
        .bytes()
        .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
    ((offset.wrapping_mul(2_654_435_761) ^ seed) % 251) as u8
}

fn bytes(id: &str, range: Range<u64>) -> Vec<u8> {
    range.map(|offset| byte_at(id, offset)).collect()
}

/// A manifest for the pretend runtimes, each detected by a pretend
/// registry key.
fn manifest(items: &[Item]) -> Manifest {
    let components: Vec<String> = items
        .iter()
        .map(|item| {
            let size = item.megabytes * 1_000_000;
            let mut hasher = sha2::Sha256::new();
            for start in (0..size).step_by(1 << 20) {
                hasher.update(bytes(item.id, start..(start + (1 << 20)).min(size)));
            }
            let sha256: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!(
                r#"{{"id": "{id}", "name": "{name}", "arch": "any", "version": "1.0",
                    "urls": ["https://demo.invalid/{id}.exe"], "size": {size}, "sha256": "{sha256}",
                    "installer": {{"type": "exe", "args": ["/quiet"]}},
                    "detect": {{"type": "registry", "key": "HKLM\\SOFTWARE\\RedistDemo\\{id}", "view": "64"}},
                    "exit_ok": [0], "exit_reboot": [3010], "exit_already": [1638], "exit_retry": [1618]}}"#,
                id = item.id,
                name = item.name,
            )
        })
        .collect();
    let json = format!(
        r#"{{"schema": 1, "version": 1, "issued": "2026-11-24T00:00:00Z", "min_client": "0.1.0",
            "latest_client": {{"version": "0.1.0", "url": "https://demo.invalid/"}},
            "components": [{}]}}"#,
        components.join(",")
    );
    Manifest::parse(json.as_bytes()).expect("the demo manifest is valid")
}

/// Which pretend runtimes are installed, shared by the machine and the
/// installers.
type Installed = Arc<Mutex<HashSet<String>>>;

/// A machine whose only registry is the set of installed demo runtimes.
struct DemoMachine(Installed);

impl Machine for DemoMachine {
    fn native_arch(&self) -> NativeArch {
        NativeArch::X64
    }

    fn known_folder(&self, _: KnownFolder) -> io::Result<PathBuf> {
        Ok(std::env::temp_dir())
    }

    fn registry_key_exists(&self, _: RegistryView, subkey: &str) -> io::Result<bool> {
        thread::sleep(Duration::from_millis(250)); // so "Checking" can be seen
        let id = subkey.rsplit('\\').next().unwrap_or_default();
        Ok(self.0.lock().unwrap().contains(id))
    }

    fn registry_value(
        &self,
        _: RegistryView,
        _: &str,
        _: &str,
    ) -> io::Result<Option<RegistryValue>> {
        Ok(None)
    }

    fn file_exists(&self, _: &Path) -> io::Result<bool> {
        Ok(false)
    }

    fn file_version(&self, _: &Path) -> io::Result<Option<Version>> {
        Ok(None)
    }

    fn subfolders(&self, _: &Path) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn run(&self, _: &Path, _: &[String]) -> io::Result<u32> {
        Err(io::Error::other("the demo runs no programs"))
    }
}

/// Serves the pretend files, slowly enough to watch.
struct DemoFetcher;

impl Fetcher for DemoFetcher {
    fn fetch(
        &self,
        url: &HttpsUrl,
        range: Range<u64>,
        _size: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let id = url
            .as_str()
            .rsplit('/')
            .next()
            .unwrap()
            .trim_end_matches(".exe");
        let chunk = 64 * 1024;
        for start in range.clone().step_by(chunk as usize) {
            let end = (start + chunk).min(range.end);
            thread::sleep(Duration::from_micros(
                (end - start) * 1_000_000 / BYTES_PER_SECOND,
            ));
            sink(&bytes(id, start..end))?;
        }
        Ok(())
    }

    fn wait(&self, duration: Duration) {
        thread::sleep(duration.min(Duration::from_secs(1)));
    }
}

/// Pretend installers: they take a while, then "install" unless their exit
/// code says otherwise.
struct DemoRunner {
    installed: Installed,
    exit_codes: Mutex<HashMap<String, VecDeque<u32>>>,
}

impl Runner for DemoRunner {
    fn run(&self, program: &Path, _: &[String], _: &Path) -> io::Result<u32> {
        let id = program.file_stem().unwrap().to_string_lossy().into_owned();
        thread::sleep(INSTALL_TIME);
        let code = {
            let mut scripts = self.exit_codes.lock().unwrap();
            let codes = scripts.get_mut(&id).expect("a demo runtime");
            if codes.len() > 1 {
                codes.pop_front().unwrap()
            } else {
                codes[0]
            }
        };
        if matches!(code, 0 | 3010) {
            self.installed.lock().unwrap().insert(id);
        }
        Ok(code)
    }

    fn wait(&self, duration: Duration) {
        thread::sleep(duration.min(Duration::from_secs(3)));
    }
}

/// Prints what the console exe would.
struct Print;

impl Reporter for Print {
    fn report(&self, event: Event<'_>) {
        if let Some(text) = event_text(event) {
            println!("{text}");
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headless = args.iter().any(|a| a == "--headless");
    let scenario = args
        .iter()
        .find(|a| *a != "--headless")
        .cloned()
        .unwrap_or_default();
    let items = items(&scenario);
    let manifest = manifest(&items);
    let id = |item: &Item| item.id.to_owned();

    let installed: Installed = Arc::new(Mutex::new(
        items.iter().filter(|i| i.present).map(id).collect(),
    ));
    let runner = DemoRunner {
        installed: installed.clone(),
        exit_codes: Mutex::new(
            items
                .iter()
                .map(|i| (id(i), i.exit_codes.iter().copied().collect()))
                .collect(),
        ),
    };

    let work_dir = std::env::temp_dir().join(format!("redist-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).expect("the temp folder is writable");
    println!("Demo log: {}", work_dir.join("redist-demo.log").display());

    let context = Context::new(
        manifest,
        Box::new(DemoMachine(installed)),
        Box::new(DemoFetcher),
        Box::new(runner),
        Box::new(|| {
            println!("The demo does not restart Windows; it only closes.");
            Ok(())
        }),
        work_dir,
        "redist-demo",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| error.to_string());

    if !headless {
        return redist::app::run(context);
    }
    let context = context.expect("the demo can start");
    let result = context
        .session(&Print)
        .run(&context.manifest, &Options::default());
    context.tidy();
    match result {
        Ok(summary) => println!("\n{}", summary_text(&context.manifest, &summary, false)),
        Err(error) => println!("\n{error}"),
    }
    Ok(())
}
