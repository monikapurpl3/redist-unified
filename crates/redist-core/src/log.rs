//! The run's log file: every decision, command and exit code, with the time.

use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;

use crate::timestamp::Timestamp;

/// A log that several threads can write to.
///
/// Writing never fails as far as the caller is concerned: a log that cannot
/// be written must not stop an installation.
pub struct Log {
    out: Mutex<Box<dyn Write + Send>>,
}

impl Log {
    /// A log that writes to `out`.
    pub fn new(out: impl Write + Send + 'static) -> Log {
        Log {
            out: Mutex::new(Box::new(out)),
        }
    }

    /// A log in a new file. Fails if the file already exists, so it can
    /// never follow a link someone left at that path.
    pub fn create(path: &Path) -> io::Result<Log> {
        Ok(Log::new(File::create_new(path)?))
    }

    /// Writes one line, after the current time.
    pub fn line(&self, text: impl fmt::Display) {
        let mut out = match self.out.lock() {
            Ok(out) => out,
            // A thread panicked while writing; the log is still usable.
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = writeln!(out, "{} {text}", Timestamp::now());
        let _ = out.flush();
    }
}

impl fmt::Debug for Log {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Log")
    }
}
