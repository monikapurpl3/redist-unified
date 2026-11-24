//! Downloading one file the way aria2 does with a metalink: in parts fetched
//! side by side, from a list of mirrors, each part resuming where it stopped.
//!
//! - A file of at least 2 MiB is split into up to [`MAX_PARTS`] byte ranges of
//!   at least [`MIN_PART`] each, fetched in parallel.
//! - Mirrors are in order of preference. Every part starts on the first one
//!   and moves on only when its mirror fails. Spreading parts over mirrors
//!   would load the fallbacks, which may be a small home server.
//! - A part that stops early continues from its last byte, on the same
//!   mirror. A mirror gets [`TRIES_PER_MIRROR`] tries in a row that bring no
//!   new bytes, with growing waits between them, before the part moves on.
//!   A try that brought bytes starts the count again.
//! - Some failures end a mirror for a part at once: not found, forbidden,
//!   the wrong size or more bytes than asked for, and no support for ranges.
//! - A mirror that ignores ranges is skipped for parts. If no mirror supports
//!   them, the whole file is fetched in one piece instead.
//! - Nothing beyond the manifest's size is accepted, from any mirror.
//!
//! Integrity comes from the SHA-256 the session checks afterwards, so parts
//! from different mirrors are as good as one download.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crate::log::Log;
use crate::manifest::{Download, HttpsUrl};

/// At most this many parts per file.
pub const MAX_PARTS: u64 = 4;

/// Parts are at least this big, so files under twice this come in one piece.
pub const MIN_PART: u64 = 1024 * 1024;

/// Tries in a row without new bytes before a part leaves a mirror.
pub const TRIES_PER_MIRROR: u32 = 3;

/// Fetches byte ranges over HTTPS.
pub trait Fetcher: Sync {
    /// Requests `range` of the file at `url`, which is `size` bytes long, and
    /// hands the bytes to `sink` in order as they arrive.
    ///
    /// The server must answer with exactly that range. An answer with the
    /// whole file is accepted only when `range` is the whole file; otherwise
    /// this fails with [`io::ErrorKind::Unsupported`]. A sink error ends the
    /// request and is returned.
    ///
    /// Errors of the kinds `NotFound`, `PermissionDenied`, `InvalidData` and
    /// `Unsupported` mean trying this URL again is pointless; any other kind
    /// may pass.
    fn fetch(
        &self,
        url: &HttpsUrl,
        range: Range<u64>,
        size: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()>;

    /// Waits before trying a mirror again.
    fn wait(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// The parts a file of `size` bytes is fetched in: contiguous, covering it.
pub fn split(size: u64) -> Vec<Range<u64>> {
    let count = (size / MIN_PART).clamp(1, MAX_PARTS);
    let length = size / count;
    (0..count)
        .map(|i| {
            let start = i * length;
            let end = if i + 1 == count { size } else { start + length };
            start..end
        })
        .collect()
}

/// Downloads `download` into the new file `dest`, calling `progress` with
/// the bytes received so far. A failed download leaves no file.
pub fn fetch_file(
    fetcher: &dyn Fetcher,
    download: &Download,
    dest: &Path,
    log: &Log,
    label: &str,
    progress: &(dyn Fn(u64) + Sync),
) -> io::Result<()> {
    let result = fetch_into(fetcher, download, dest, log, label, progress);
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

fn fetch_into(
    fetcher: &dyn Fetcher,
    download: &Download,
    dest: &Path,
    log: &Log,
    label: &str,
    progress: &(dyn Fn(u64) + Sync),
) -> io::Result<()> {
    File::create_new(dest)?.set_len(download.size)?;
    let job = Job {
        fetcher,
        download,
        dest,
        log,
        label,
        progress,
        received: AtomicU64::new(0),
        no_ranges: Mutex::new(vec![false; download.urls.len()]),
    };
    let parts = split(download.size);
    let results: Vec<io::Result<()>> = if parts.len() == 1 {
        vec![job.fetch_part(parts[0].clone())]
    } else {
        thread::scope(|scope| {
            let threads: Vec<_> = parts
                .iter()
                .map(|part| scope.spawn(|| job.fetch_part(part.clone())))
                .collect();
            threads
                .into_iter()
                .map(|thread| {
                    thread
                        .join()
                        .unwrap_or_else(|_| Err(io::Error::other("a download thread panicked")))
                })
                .collect()
        })
    };
    let Some(error) = results.into_iter().find_map(Result::err) else {
        return Ok(());
    };
    let no_mirror_has_ranges = job
        .no_ranges
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .all(|&no| no);
    if parts.len() > 1 && no_mirror_has_ranges {
        log.line(format_args!(
            "{label}: no mirror sends parts of files; fetching it in one piece"
        ));
        job.received.store(0, Ordering::Relaxed);
        return job.fetch_part(0..download.size);
    }
    Err(error)
}

/// One file being downloaded.
struct Job<'a> {
    fetcher: &'a dyn Fetcher,
    download: &'a Download,
    dest: &'a Path,
    log: &'a Log,
    label: &'a str,
    progress: &'a (dyn Fn(u64) + Sync),
    /// Bytes received over all parts.
    received: AtomicU64,
    /// Mirrors that answered a part with the whole file.
    no_ranges: Mutex<Vec<bool>>,
}

impl Job<'_> {
    fn fetch_part(&self, part: Range<u64>) -> io::Result<()> {
        let whole = part.start == 0 && part.end == self.download.size;
        let mut file = OpenOptions::new().write(true).open(self.dest)?;
        let mut position = part.start;
        let mut mirror = 0;
        let mut fruitless = 0;
        let mut last_error = None;
        while position < part.end {
            let Some(url) = self.download.urls.get(mirror) else {
                return Err(last_error.unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "no mirror sends parts of files")
                }));
            };
            if !whole
                && self
                    .no_ranges
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)[mirror]
            {
                mirror += 1;
                continue;
            }

            file.seek(SeekFrom::Start(position))?;
            let start = position;
            let result =
                self.fetcher
                    .fetch(url, position..part.end, self.download.size, &mut |bytes| {
                        let end = position + bytes.len() as u64;
                        if end > part.end {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "the server sent more than was asked for",
                            ));
                        }
                        file.write_all(bytes)?;
                        position = end;
                        let received = self
                            .received
                            .fetch_add(bytes.len() as u64, Ordering::Relaxed)
                            + bytes.len() as u64;
                        (self.progress)(received);
                        Ok(())
                    });
            let error = match result {
                Ok(()) if position == part.end => break,
                Ok(()) => {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "the connection ended early")
                }
                Err(error) => error,
            };
            self.log.line(format_args!(
                "{}: {url}, bytes {start}-{}: {error} after {} new bytes",
                self.label,
                part.end - 1,
                position - start
            ));

            let kind = error.kind();
            if kind == io::ErrorKind::Unsupported {
                self.no_ranges
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)[mirror] = true;
            }
            fruitless = if position > start { 0 } else { fruitless + 1 };
            if is_final(kind) || fruitless >= TRIES_PER_MIRROR {
                mirror += 1;
                fruitless = 0;
            } else if fruitless > 0 {
                self.fetcher.wait(backoff(fruitless));
            }
            last_error = Some(io::Error::new(kind, format!("{error} ({url})")));
        }
        file.flush()
    }
}

/// Whether an error ends a mirror for a part at once.
fn is_final(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::NotFound
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::InvalidData
            | io::ErrorKind::Unsupported
    )
}

/// The wait after the n-th fruitless try in a row: 1, 2, 4 seconds.
pub fn backoff(fruitless: u32) -> Duration {
    Duration::from_secs(1 << fruitless.saturating_sub(1).min(4))
}

#[cfg(test)]
mod tests {
    use super::{MIN_PART, backoff, split};
    use std::time::Duration;

    #[test]
    fn parts_cover_the_file() {
        for size in [1, 2 * MIN_PART - 1] {
            let parts = split(size);
            assert_eq!((parts.len(), parts[0].clone()), (1, 0..size));
        }
        assert_eq!(
            split(3 * MIN_PART + 5),
            [
                0..MIN_PART + 1,
                MIN_PART + 1..2 * MIN_PART + 2,
                2 * MIN_PART + 2..3 * MIN_PART + 5
            ]
        );
        let parts = split(100 * MIN_PART + 3);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts[3].end, 100 * MIN_PART + 3);
        assert!(parts.windows(2).all(|pair| pair[0].end == pair[1].start));
    }

    #[test]
    fn waits_grow() {
        assert_eq!([1, 2, 3].map(backoff), [1, 2, 4].map(Duration::from_secs));
    }
}
