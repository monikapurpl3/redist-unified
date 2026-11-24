//! The download engine against a fake server whose mirrors misbehave on
//! purpose.

use std::collections::HashMap;
use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use redist_core::download::{Fetcher, MIN_PART, TRIES_PER_MIRROR, fetch_file, split};
use redist_core::log::Log;
use redist_core::manifest::{Download, HttpsUrl, Sha256};

/// How one mirror behaves.
#[derive(Clone, Copy, Debug)]
enum Mirror {
    /// Serves what is asked.
    Good,
    /// Breaks the connection after `after` bytes, for its first `times`
    /// requests (over all parts).
    Drops { after: u64, times: usize },
    /// Times out, always.
    Down,
    /// Answers 404, always.
    Missing,
    /// Answers every request with the whole file.
    NoRanges,
    /// Sends ten bytes more than asked.
    Overflows,
}

struct FakeServer {
    data: Vec<u8>,
    mirrors: Vec<(String, Mirror)>,
    requests: Mutex<Vec<(String, Range<u64>)>>,
    drops: Mutex<HashMap<String, usize>>,
    waits: Mutex<Vec<Duration>>,
}

impl FakeServer {
    fn new(size: usize, mirrors: &[Mirror]) -> Self {
        let data = (0..size).map(|i| (i * 31 % 251) as u8).collect();
        let mirrors = mirrors
            .iter()
            .enumerate()
            .map(|(i, &m)| (format!("https://mirror{i}.example/file.exe"), m))
            .collect();
        FakeServer {
            data,
            mirrors,
            requests: Mutex::new(Vec::new()),
            drops: Mutex::new(HashMap::new()),
            waits: Mutex::new(Vec::new()),
        }
    }

    fn download(&self) -> Download {
        Download {
            urls: self
                .mirrors
                .iter()
                .map(|(url, _)| HttpsUrl::try_from(url.clone()).unwrap())
                .collect(),
            size: self.data.len() as u64,
            // The engine does not check the hash; the session does.
            sha256: Sha256::from_bytes([0; 32]),
        }
    }

    /// The ranges asked of a mirror, sorted. Parts run in parallel, so the
    /// order they were asked in varies from run to run.
    fn requests_to(&self, mirror: usize) -> Vec<Range<u64>> {
        let mut ranges = self.requests_in_order(mirror);
        ranges.sort_by_key(|range| (range.start, range.end));
        ranges
    }

    /// The ranges asked of a mirror, in the order they were asked.
    fn requests_in_order(&self, mirror: usize) -> Vec<Range<u64>> {
        let url = &self.mirrors[mirror].0;
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(u, _)| u == url)
            .map(|(_, range)| range.clone())
            .collect()
    }

    fn waits(&self) -> Vec<Duration> {
        let mut waits = self.waits.lock().unwrap().clone();
        waits.sort();
        waits
    }
}

/// Hands `bytes` over in 64 KiB pieces, like a network would.
fn send(sink: &mut dyn FnMut(&[u8]) -> io::Result<()>, bytes: &[u8]) -> io::Result<()> {
    bytes.chunks(64 * 1024).try_for_each(sink)
}

impl Fetcher for FakeServer {
    fn fetch(
        &self,
        url: &HttpsUrl,
        range: Range<u64>,
        size: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        assert_eq!(size, self.data.len() as u64);
        self.requests
            .lock()
            .unwrap()
            .push((url.as_str().to_owned(), range.clone()));
        let mirror = self
            .mirrors
            .iter()
            .find(|(u, _)| u == url.as_str())
            .unwrap()
            .1;
        let part = &self.data[range.start as usize..range.end as usize];
        match mirror {
            Mirror::Good => send(sink, part),
            Mirror::Drops { after, times } => {
                let drop_now = {
                    let mut drops = self.drops.lock().unwrap();
                    let done = drops.entry(url.as_str().to_owned()).or_default();
                    *done += 1;
                    *done <= times
                };
                if drop_now {
                    send(sink, &part[..(after as usize).min(part.len())])?;
                    Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset"))
                } else {
                    send(sink, part)
                }
            }
            Mirror::Down => Err(io::Error::new(io::ErrorKind::TimedOut, "timed out")),
            Mirror::Missing => Err(io::Error::new(io::ErrorKind::NotFound, "HTTP 404")),
            Mirror::NoRanges if range == (0..size) => send(sink, part),
            Mirror::NoRanges => Err(io::Error::new(io::ErrorKind::Unsupported, "no ranges")),
            Mirror::Overflows => {
                send(sink, part)?;
                send(sink, &[0; 10])
            }
        }
    }

    fn wait(&self, duration: Duration) {
        self.waits.lock().unwrap().push(duration);
    }
}

/// A path in the temp folder for one test, deleted afterwards.
struct Dest(PathBuf);

impl Dest {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("redist-download-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Dest(path)
    }
}

impl Drop for Dest {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Downloads from `server` into `dest`. Returns the result and the most
/// bytes progress reported.
fn fetch(server: &FakeServer, dest: &Dest) -> (io::Result<()>, u64) {
    let most = AtomicU64::new(0);
    let result = fetch_file(
        server,
        &server.download(),
        &dest.0,
        &Log::new(io::sink()),
        "test",
        &|received| {
            most.fetch_max(received, Ordering::Relaxed);
        },
    );
    (result, most.into_inner())
}

#[track_caller]
fn assert_downloaded(server: &FakeServer, dest: &Dest) {
    let (result, most) = fetch(server, dest);
    result.unwrap();
    assert!(
        std::fs::read(&dest.0).unwrap() == server.data,
        "wrong bytes"
    );
    assert_eq!(most, server.data.len() as u64);
}

const SIZE: usize = 5 * MIN_PART as usize + 12_345;

#[test]
fn a_big_file_comes_in_parts() {
    let server = FakeServer::new(SIZE, &[Mirror::Good]);
    let dest = Dest::new("parts");
    assert_downloaded(&server, &dest);
    assert_eq!(server.requests_to(0), split(SIZE as u64));
    assert_eq!(server.requests_to(0).len(), 4);
}

#[test]
fn a_small_file_comes_in_one_piece() {
    let server = FakeServer::new(100_000, &[Mirror::Good]);
    let dest = Dest::new("small");
    assert_downloaded(&server, &dest);
    let requests = server.requests_to(0);
    assert_eq!((requests.len(), requests[0].clone()), (1, 0..100_000));
}

#[test]
fn dropped_parts_resume_where_they_stopped() {
    let server = FakeServer::new(
        SIZE,
        &[Mirror::Drops {
            after: 300_000,
            times: 2,
        }],
    );
    let dest = Dest::new("resume");
    assert_downloaded(&server, &dest);

    let parts = split(SIZE as u64);
    let requests = server.requests_to(0);
    assert_eq!(requests.len(), parts.len() + 2);
    // Every request runs to the end of its part, and the two extra ones
    // start where a dropped one stopped.
    let resumed: Vec<_> = requests
        .iter()
        .filter(|r| !parts.iter().any(|p| p.start == r.start))
        .collect();
    assert_eq!(resumed.len(), 2);
    for range in resumed {
        let part = parts.iter().find(|p| p.contains(&range.start)).unwrap();
        assert_eq!(range.end, part.end);
        assert_eq!((range.start - part.start) % 300_000, 0);
    }
    // A try that brought bytes is no reason to wait.
    assert!(server.waits().is_empty());
}

#[test]
fn a_dead_mirror_hands_its_parts_to_the_next() {
    let server = FakeServer::new(SIZE, &[Mirror::Down, Mirror::Good]);
    let dest = Dest::new("dead");
    assert_downloaded(&server, &dest);

    let parts = split(SIZE as u64);
    assert_eq!(
        server.requests_to(0).len(),
        parts.len() * TRIES_PER_MIRROR as usize
    );
    assert_eq!(server.requests_to(1), parts);
    // Each part waited 1 and then 2 seconds before giving up on mirror 0.
    let one_part = [Duration::from_secs(1), Duration::from_secs(2)];
    let mut expected: Vec<_> = (0..parts.len()).flat_map(|_| one_part).collect();
    expected.sort();
    assert_eq!(server.waits(), expected);
}

#[test]
fn a_missing_file_is_not_asked_for_twice() {
    let server = FakeServer::new(SIZE, &[Mirror::Missing, Mirror::Good]);
    let dest = Dest::new("missing");
    assert_downloaded(&server, &dest);
    assert_eq!(server.requests_to(0), split(SIZE as u64));
    assert!(server.waits().is_empty());
}

#[test]
fn a_mirror_without_ranges_is_skipped_for_parts() {
    let server = FakeServer::new(SIZE, &[Mirror::NoRanges, Mirror::Good]);
    let dest = Dest::new("skip-no-ranges");
    assert_downloaded(&server, &dest);
    assert_eq!(server.requests_to(1), split(SIZE as u64));
    assert!(
        !server.requests_to(0).contains(&(0..SIZE as u64)),
        "the whole file was fetched although a mirror sends parts"
    );
}

#[test]
fn without_ranges_anywhere_the_file_comes_whole() {
    let server = FakeServer::new(SIZE, &[Mirror::NoRanges]);
    let dest = Dest::new("whole");
    assert_downloaded(&server, &dest);
    // The parts that asked before the first refusal was known were refused
    // (which ones varies); after them all comes one request for everything.
    let requests = server.requests_in_order(0);
    assert_eq!(requests.last().unwrap(), &(0..SIZE as u64));
    assert_eq!(
        requests.iter().filter(|r| **r == (0..SIZE as u64)).count(),
        1
    );
    assert!((2..=5).contains(&requests.len()), "{requests:?}");
}

#[test]
fn bytes_past_a_part_are_never_written() {
    // Each part arrives whole, then ten more bytes that belong to the next
    // part's range. They are refused, so no part spills into its
    // neighbour, and the complete parts need no other mirror.
    let server = FakeServer::new(SIZE, &[Mirror::Overflows, Mirror::Good]);
    let dest = Dest::new("overflow");
    assert_downloaded(&server, &dest);
    assert!(server.requests_to(1).is_empty());
}

#[test]
fn when_every_mirror_fails_nothing_is_left() {
    let server = FakeServer::new(SIZE, &[Mirror::Down, Mirror::Missing]);
    let dest = Dest::new("all-fail");
    let error = fetch(&server, &dest).0.unwrap_err();
    // The last mirror's error, with its URL.
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(
        error
            .to_string()
            .contains("https://mirror1.example/file.exe"),
        "{error}"
    );
    assert!(!dest.0.exists());
}

#[test]
fn the_destination_must_be_new() {
    let server = FakeServer::new(100, &[Mirror::Good]);
    let dest = Dest::new("exists");
    std::fs::write(&dest.0, b"planted").unwrap();
    let error = fetch(&server, &dest).0.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(server.requests_to(0).is_empty());
}
