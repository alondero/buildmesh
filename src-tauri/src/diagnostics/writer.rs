//! A size-bounded, self-rotating file writer, used for both the diagnostics
//! log and the main tracing log.
//!
//! Both logs exist to help diagnose a disk-fill / resource grind, so neither
//! may *cause* one: each file is capped at a byte limit and a bounded number
//! of rotated generations is kept, bounding total on-disk size to
//! `max_bytes * (keep + 1)`. Rotation is by **bytes**, not time — a single
//! `debug`-level build storm can write a lot in one wall-clock hour, so a
//! time-based cap (e.g. hourly files) would not actually bound the size.
//!
//! The core is a plain [`std::io::Write`] so it can back the async
//! `tracing_appender::non_blocking` wrapper for the main log (which keeps its
//! fixed `buildmesh.log` name — the `/use`, `/verify`, `/verify-ui` skills and
//! the `scripts/*log*.ps1` helpers tail that exact path). The diagnostics
//! sampler additionally uses [`RotatingWriter::write_line`], which prepends a
//! timestamp and periodically `sync_all`s so the timeline survives a hard
//! reboot.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Default per-file byte cap before rotation (diagnostics log). 16 MiB ≈ days
/// at the 5 s cadence (~150 bytes/line), so rotation is rare but bounded.
pub const DIAG_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Default rotated generations to keep for the diagnostics log.
pub const DIAG_KEEP: usize = 1;

/// `sync_all` cadence in lines for [`write_line`]. Per-line `sync_all` would
/// stall the writer on a thrashing disk (exactly when we most need to keep
/// sampling); flushing every line but fsyncing every Nth bounds worst-case
/// loss to N samples while avoiding the stall.
const SYNC_EVERY: u64 = 6;

pub struct RotatingWriter {
    path: PathBuf,
    file: File,
    written: u64,
    max_bytes: u64,
    keep: usize,
    lines_since_sync: u64,
}

impl RotatingWriter {
    /// Open (creating if absent, appending if present) with explicit limits.
    pub fn with_limits(path: PathBuf, max_bytes: u64, keep: usize) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            file,
            written,
            max_bytes,
            keep,
            lines_since_sync: 0,
        })
    }

    /// Open the diagnostics log with its default limits.
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        Self::with_limits(path, DIAG_MAX_BYTES, DIAG_KEEP)
    }

    /// Write one diagnostics line (a trailing newline is added). Prepends an
    /// RFC3339 UTC timestamp so the log is self-describing without a paired
    /// subscriber. Flushes every line and `sync_all`s every [`SYNC_EVERY`].
    pub fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let stamped = format!("{} {}\n", chrono::Utc::now().to_rfc3339(), line);
        self.write_all(stamped.as_bytes())?;
        self.flush()?;
        self.lines_since_sync += 1;
        if self.lines_since_sync >= SYNC_EVERY {
            let _ = self.file.sync_all();
            self.lines_since_sync = 0;
        }
        Ok(())
    }

    /// Rotate `path` → `path.1` (shuffling existing generations up and dropping
    /// the oldest), then reopen a fresh empty `path`.
    fn rotate(&mut self) -> std::io::Result<()> {
        // Flush + fsync the current file before it becomes `.1` so nothing is
        // lost across the rename.
        let _ = self.file.flush();
        let _ = self.file.sync_all();

        // Drop the oldest, then shift each generation up by one.
        let oldest = generation_path(&self.path, self.keep);
        let _ = std::fs::remove_file(&oldest);
        for gen in (1..self.keep).rev() {
            let from = generation_path(&self.path, gen);
            let to = generation_path(&self.path, gen + 1);
            let _ = std::fs::rename(&from, &to);
        }
        // Current → `.1`.
        let _ = std::fs::rename(&self.path, generation_path(&self.path, 1));

        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.written = 0;
        self.lines_since_sync = 0;
        Ok(())
    }
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Rotate *before* a write that would cross the cap. A single write
        // larger than `max_bytes` still lands (after a rotation) rather than
        // erroring — a lone oversized line is preferable to a dropped log.
        if self.written > 0 && self.written + buf.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// `foo.log` + generation `n` → `foo.log.n`.
fn generation_path(path: &Path, gen: usize) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(".{gen}"));
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch path per test so cargo's parallel runner can't collide.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("bm-diag-{name}.log"))
    }

    fn cleanup(path: &Path, keep: usize) {
        let _ = std::fs::remove_file(path);
        for gen in 1..=keep + 2 {
            let _ = std::fs::remove_file(generation_path(path, gen));
        }
    }

    #[test]
    fn generation_path_appends_suffix() {
        let p = Path::new("C:/logs/diagnostics.log");
        assert_eq!(generation_path(p, 1), PathBuf::from("C:/logs/diagnostics.log.1"));
        assert_eq!(generation_path(p, 3), PathBuf::from("C:/logs/diagnostics.log.3"));
    }

    #[test]
    fn writes_are_appended_and_stamped() {
        let path = scratch("append");
        cleanup(&path, 1);
        {
            let mut w = RotatingWriter::open(path.clone()).unwrap();
            w.write_line("DIAG hello=1").unwrap();
            w.write_line("DIAG hello=2").unwrap();
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "both lines must be present");
        assert!(lines[0].contains("DIAG hello=1"));
        assert!(lines[1].contains("DIAG hello=2"));
        // Each line is timestamp-prefixed (RFC3339 starts with a 4-digit year).
        assert!(
            lines[0].split(' ').next().unwrap().len() >= 20,
            "line must be timestamp-prefixed, got: {}",
            lines[0]
        );
        cleanup(&path, 1);
    }

    #[test]
    fn reopen_appends_rather_than_truncates() {
        let path = scratch("reopen");
        cleanup(&path, 1);
        {
            let mut w = RotatingWriter::open(path.clone()).unwrap();
            w.write_line("DIAG first=1").unwrap();
        }
        {
            // A second session (app restart) must not clobber the prior run.
            let mut w = RotatingWriter::open(path.clone()).unwrap();
            w.write_line("DIAG second=1").unwrap();
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("first=1"), "prior session must survive reopen");
        assert!(contents.contains("second=1"));
        cleanup(&path, 1);
    }

    /// The safety-critical property: writing past the byte cap rotates the
    /// current file to `.1` and starts a fresh one, so the live file never
    /// exceeds `max_bytes` by more than one line. Keep=1 ⇒ at most `.1` exists.
    #[test]
    fn rotates_at_byte_cap_keeping_one_generation() {
        let path = scratch("rotate-keep1");
        cleanup(&path, 1);
        // Tiny cap so a handful of ~30-byte lines forces rotation.
        let mut w = RotatingWriter::with_limits(path.clone(), 64, 1).unwrap();
        for i in 0..20 {
            w.write_line(&format!("DIAG n={i:04}")).unwrap();
        }
        drop(w);

        let live_len = std::fs::metadata(&path).unwrap().len();
        assert!(
            live_len <= 64 + 64, // cap + one oversized line of slack
            "live file must stay near the cap after rotation, got {live_len} bytes"
        );
        assert!(
            generation_path(&path, 1).exists(),
            "the rotated `.1` generation must exist"
        );
        assert!(
            !generation_path(&path, 2).exists(),
            "keep=1 must never leave a `.2` generation"
        );
        cleanup(&path, 1);
    }

    /// Keep=2 exercises the generation-shuffle loop: repeated rotation must
    /// leave exactly `.1` and `.2`, never `.3`.
    #[test]
    fn rotates_keeping_two_generations_shuffles_and_drops_oldest() {
        let path = scratch("rotate-keep2");
        cleanup(&path, 2);
        let mut w = RotatingWriter::with_limits(path.clone(), 48, 2).unwrap();
        for i in 0..40 {
            w.write_line(&format!("DIAG n={i:04}")).unwrap();
        }
        drop(w);

        assert!(generation_path(&path, 1).exists(), "`.1` must exist");
        assert!(generation_path(&path, 2).exists(), "`.2` must exist");
        assert!(
            !generation_path(&path, 3).exists(),
            "keep=2 must drop the oldest — no `.3`"
        );
        cleanup(&path, 2);
    }
}
