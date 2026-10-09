//! Fuzz harness for the embedded HTTP server's request-read path (issue #2156).
//!
//! [`SECURITY.md`](../../../SECURITY.md) puts credential leaks and hostile
//! input on the remote-access server in scope for researchers. The read path
//! is the seam they attack, so it is the seam this harness drives, end to end
//! and without re-implementing any of it: raw client bytes →
//! [`server::read_request_head`] → [`router::content_length`] →
//! [`request::read_body_with_cap`]. `SECURITY.md` and
//! `docs/knowledge-primer.md` both name that body read as *the* single seam
//! every body-reading route must go through, which is what makes a
//! coverage-free harness worth having here.
//!
//! # Running it
//!
//! Smoke run — the default, and what the Rust workflow in
//! `.github/workflows/verify.yml` executes:
//!
//! ```text
//! cd src-tauri && cargo test --lib http::fuzz
//! ```
//!
//! Longer campaign:
//!
//! ```text
//! cd src-tauri && BUILDMESH_FUZZ_CASES=500000 cargo test --release --lib http::fuzz -- --nocapture
//! ```
//!
//! `BUILDMESH_FUZZ_SEED` (default `0x2156_0005`) selects the PRNG seed, so a
//! campaign reproduces exactly from the printed seed.
//!
//! # Corpus and invariants
//!
//! `src-tauri/fuzz/corpus/http_request/*.bin` holds raw request byte streams.
//! `corpus_matches_pinned_outcomes` pins the outcome of every seed — including
//! the four streams the `read_body_with_cap` unit tests send over loopback TCP
//! (`oversized-content-length`, `early-eof-content-length`, `exact-length-body`,
//! `zero-length-body`) — so this is a regression suite, not an empty harness.
//! `mutated_inputs_preserve_read_invariants` then runs seeded byte-level
//! mutations of those seeds through the same read path.
//!
//! A finding is: a panic in the reader; a [`ReadBodyError::TimedOut`] (the
//! harness closes the client half, so no read may ever wait out
//! [`BODY_READ_TIMEOUT`]); a returned body that is not the exact run of request
//! bytes sitting between the head and whatever the stream still has unread, is
//! not exactly `Content-Length` bytes, or exceeds the route cap; a
//! [`ReadBodyError::TooLarge`] for a length within the cap; a
//! [`ReadBodyError::ReadFailed`] with no advertised body; or a head larger than
//! [`MAX_HEADER_BYTES`] that did not report overflow. Failing inputs are
//! written to `.tmp/fuzz-artifacts/` under a process-unique name and the run
//! panics with the input inline.
//!
//! # Why this instead of `cargo-fuzz`
//!
//! `cargo fuzz` needs a nightly toolchain, and both the repo's local toolchain
//! set and `.github/workflows/verify.yml` pin stable — a `cargo-fuzz` target
//! could not be built or run in either place, so it could not be verified here.
//! This harness is the stable equivalent: seeded mutations over a checked-in
//! corpus, run by `cargo test`. A coverage-guided `cargo-fuzz` target can
//! replace [`mutate`]'s mutation loop without touching the reader — both would
//! call the same three functions.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufStream};
use tokio::runtime::Runtime;

use super::request::{read_body_with_cap, ReadBodyError, BODY_READ_TIMEOUT};
use super::router::{content_length, MAX_HEADER_BYTES};
use super::server::read_request_head;

/// Route caps every input is driven under. `0` and `5` reach the `TooLarge`
/// arm from either side of a 6-byte body, `6` and `8` are the inclusive and
/// loose side of the same boundary, `1024` is the order of magnitude of a real
/// body route, and `64 KiB` is the largest cap in the route table — so the
/// "advertised length within cap but never actually sent" case is driven too.
const CAPS: [usize; 6] = [0, 5, 6, 8, 1024, 64 * 1024];

/// Inputs longer than this are truncated before driving. Sized above
/// [`MAX_HEADER_BYTES`] so the header-overflow arm stays reachable.
const MAX_INPUT_BYTES: usize = 80 * 1024;

/// Duplex buffer. Must exceed [`MAX_INPUT_BYTES`] or `write_all` would block
/// instead of delivering the whole request.
const DUPLEX_CAPACITY: usize = 128 * 1024;

/// Wall-clock ceiling for one drive, far below [`BODY_READ_TIMEOUT`]. The
/// client half is closed before any read, so hitting this ceiling is itself a
/// finding: the reader stopped honouring EOF.
const DRIVE_TIMEOUT: Duration = Duration::from_secs(5);

const DEFAULT_CASES: usize = 2_048;
const DEFAULT_SEED: u64 = 0x2156_0005;

/// What one drive of one input under one cap produced.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// No usable head (EOF before a byte, a head that is not valid UTF-8, or
    /// no bytes before the head deadline). Production drops the connection.
    HeadRejected,
    /// Head larger than [`MAX_HEADER_BYTES`]. Production answers `431` and
    /// returns **without** reading a body.
    HeadOverflow,
    Body {
        /// `Content-Length` as [`content_length`] parsed it.
        content_length: usize,
        /// `Content-Length: 0` short-circuits, everything else reads.
        body: Option<Result<Vec<u8>, ReadBodyError>>,
        /// Bytes still readable from the stream once the body read finished
        /// (a pipelined request, or trailing junk the client sent). Measured
        /// by draining the stream, which the harness may do only because the
        /// client half is already closed.
        leftover: usize,
    },
}

#[derive(Debug)]
struct Run {
    cap: usize,
    outcome: Outcome,
    /// `request_line.len() + headers.len()` as the head read saw it.
    head_bytes: usize,
    overflow: bool,
}

impl Run {
    fn observed(&self) -> Observed {
        match &self.outcome {
            Outcome::HeadRejected => Observed::HeadRejected,
            Outcome::HeadOverflow => Observed::HeadOverflow,
            Outcome::Body { body, .. } => match body {
                None => Observed::Body(Vec::new()),
                Some(Ok(buf)) => Observed::Body(buf.clone()),
                Some(Err(ReadBodyError::TooLarge)) => Observed::TooLarge,
                Some(Err(ReadBodyError::ReadFailed)) => Observed::ReadFailed,
                Some(Err(ReadBodyError::TimedOut)) => Observed::TimedOut,
            },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Expected {
    HeadRejected,
    Body(&'static [u8]),
    TooLarge,
    ReadFailed,
}

/// What a run produced, in the shape the pinned table is written in.
#[derive(Debug, PartialEq, Eq)]
enum Observed {
    HeadRejected,
    HeadOverflow,
    TooLarge,
    ReadFailed,
    TimedOut,
    Body(Vec<u8>),
}

impl Expected {
    fn as_observed(&self) -> Observed {
        match self {
            Expected::HeadRejected => Observed::HeadRejected,
            Expected::Body(bytes) => Observed::Body(bytes.to_vec()),
            Expected::TooLarge => Observed::TooLarge,
            Expected::ReadFailed => Observed::ReadFailed,
        }
    }
}

struct Seed {
    /// File name under `src-tauri/fuzz/corpus/http_request/`.
    file: &'static str,
    cap: usize,
    expected: Expected,
}

/// Pinned outcome per corpus file. Every entry is a literal expected value,
/// not a tautology: `exact-length-body` is the `abc123` stream the
/// `read_body_with_cap_reads_exact_bytes_on_happy_path` unit test sends, and
/// `early-eof-content-length` is that test's `ReadFailed` stream.
const SEEDS: &[Seed] = &[
    Seed {
        file: "exact-length-body.bin",
        cap: 1024,
        expected: Expected::Body(b"abc123"),
    },
    Seed {
        file: "exact-length-body.bin",
        cap: 6,
        expected: Expected::Body(b"abc123"),
    },
    Seed {
        file: "exact-length-body.bin",
        cap: 0,
        expected: Expected::TooLarge,
    },
    Seed {
        file: "exact-length-body.bin",
        cap: 5,
        expected: Expected::TooLarge,
    },
    Seed {
        file: "zero-length-body.bin",
        cap: 1024,
        expected: Expected::Body(b""),
    },
    // The unit test draws the line at 10 bytes for this 11-byte body; the
    // harness sweeps 8, which is the same side of the boundary.
    Seed {
        file: "oversized-content-length.bin",
        cap: 8,
        expected: Expected::TooLarge,
    },
    Seed {
        file: "early-eof-content-length.bin",
        cap: 1024,
        expected: Expected::ReadFailed,
    },
    // Truncated head: the request line arrives, the header block never
    // terminates, and the body read must be a no-op rather than a guess.
    Seed {
        file: "truncated-header-block.bin",
        cap: 1024,
        expected: Expected::Body(b""),
    },
    // `Content-Length: 18446744073709551616` is one past `u64::MAX`, so the
    // parse fails and the body read must not allocate. The allocation
    // happens *before* any byte arrives, so an unparsed length that fell
    // through to `usize` would be the allocation-DoS this guards.
    Seed {
        file: "content-length-overflow.bin",
        cap: 1024,
        expected: Expected::Body(b""),
    },
    Seed {
        file: "content-length-negative.bin",
        cap: 1024,
        expected: Expected::Body(b""),
    },
    Seed {
        file: "content-length-trailing-junk.bin",
        cap: 1024,
        expected: Expected::Body(b""),
    },
    Seed {
        file: "content-length-hex.bin",
        cap: 1024,
        expected: Expected::Body(b""),
    },
    // `extract_header_value` trims the value, so whitespace around a length
    // is tolerated.
    Seed {
        file: "content-length-padded-value.bin",
        cap: 1024,
        expected: Expected::Body(b"abc123"),
    },
    // `usize::from_str` accepts a leading `+`, so this one *is* 6 bytes.
    Seed {
        file: "content-length-leading-plus.bin",
        cap: 1024,
        expected: Expected::Body(b"abc123"),
    },
    // `extract_header_value` returns the first match, so the second
    // `Content-Length` never reaches the read.
    Seed {
        file: "duplicate-content-length.bin",
        cap: 1024,
        expected: Expected::Body(b"abcdef"),
    },
    // A NUL byte inside the length value: valid UTF-8, unparseable number.
    Seed {
        file: "nul-byte-content-length.bin",
        cap: 1024,
        expected: Expected::Body(b""),
    },
    Seed {
        file: "nul-body-passthrough.bin",
        cap: 1024,
        expected: Expected::Body(b"a\0b\0"),
    },
    // Invalid UTF-8 in the request line: `read_line` fails, head rejected.
    Seed {
        file: "invalid-utf8-request-line.bin",
        cap: 1024,
        expected: Expected::HeadRejected,
    },
    // A multi-byte body: the length is counted in bytes, not characters.
    Seed {
        file: "utf8-body-byte-length.bin",
        cap: 1024,
        expected: Expected::Body("héllo".as_bytes()),
    },
];

/// Non-file seed: padded past [`MAX_HEADER_BYTES`] so the `431` arm is
/// driven. Synthesised rather than checked in because the corpus directory
/// stays small and every file in it is a real byte stream.
fn header_overflow_seed() -> Vec<u8> {
    let mut input = b"POST /api/meshes HTTP/1.1\r\nHost: localhost\r\n".to_vec();
    while input.len() <= MAX_HEADER_BYTES {
        input.extend_from_slice(b"X-Pad: ");
        input.resize(input.len() + 512, b'a');
        input.extend_from_slice(b"\r\n");
    }
    input.extend_from_slice(b"\r\n");
    input
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/http_request")
}

/// Every corpus file, with its name. A file with no entry in [`SEEDS`] is a
/// regression nobody pinned — `corpus_files_all_have_a_pinned_outcome` fails.
fn load_corpus() -> Vec<(String, Vec<u8>)> {
    let dir = corpus_dir();
    let mut entries: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read corpus dir {}: {e}", dir.display()))
        .map(|entry| {
            let path = entry.expect("corpus entry").path();
            let name = path
                .file_name()
                .expect("corpus file name")
                .to_string_lossy()
                .into_owned();
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("read corpus {}: {e}", path.display()));
            (name, bytes)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        !entries.is_empty(),
        "corpus dir {} is empty — the target would be an empty harness",
        dir.display()
    );
    entries
}

fn cases_from_env() -> usize {
    match std::env::var("BUILDMESH_FUZZ_CASES") {
        Ok(raw) => raw
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("BUILDMESH_FUZZ_CASES must be a count: {e}")),
        Err(_) => DEFAULT_CASES,
    }
}

fn seed_from_env() -> u64 {
    let raw = std::env::var("BUILDMESH_FUZZ_SEED").unwrap_or_else(|_| format!("{DEFAULT_SEED}"));
    let raw = raw.trim();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).unwrap_or_else(|e| panic!("BUILDMESH_FUZZ_SEED: {e}"))
    } else {
        raw.parse()
            .unwrap_or_else(|e| panic!("BUILDMESH_FUZZ_SEED must be u64 or 0x-hex: {e}"))
    }
}

/// Drive one input under one cap. The raw input goes into an in-memory duplex
/// and is closed before the server side reads, so every read sees real EOF
/// rather than a stall.
fn run_cap(rt: &Runtime, input: &[u8], cap: usize) -> Run {
    let (mut client, server) = tokio::io::duplex(DUPLEX_CAPACITY);
    let mut server = BufStream::new(server);
    rt.block_on(async {
        tokio::time::timeout(DRIVE_TIMEOUT, async {
            // A short write would silently narrow what this drive covers, so
            // treat it as the harness bug it is instead of continuing with a
            // truncated request.
            if client.write_all(input).await.is_err() {
                panic!(
                    "the duplex refused {} bytes against a {DUPLEX_CAPACITY}-byte capacity; \
                     MAX_INPUT_BYTES must stay below it",
                    input.len()
                );
            }
            drop(client);
            let Some((request_line, headers, overflow)) = read_request_head(&mut server).await
            else {
                return Run {
                    cap,
                    outcome: Outcome::HeadRejected,
                    head_bytes: 0,
                    overflow: false,
                };
            };
            let head_bytes = request_line.len() + headers.len();
            if overflow {
                return Run {
                    cap,
                    outcome: Outcome::HeadOverflow,
                    head_bytes,
                    overflow,
                };
            }
            let cl = content_length(&headers);
            // `Content-Length: 0` never touches the stream, so the read
            // is only attempted for a non-zero length.
            let body = if cl == 0 {
                None
            } else {
                Some(read_body_with_cap(&mut server, cl, cap).await)
            };
            // Whatever is still readable sits immediately *after* the body
            // in the byte stream, so counting it locates the body exactly
            // without the harness having to re-derive where the head read
            // stopped.
            let mut leftover = 0usize;
            let mut chunk = [0u8; 4096];
            loop {
                match server.read(&mut chunk).await {
                    // An in-memory duplex cannot error, and counting fewer
                    // bytes than are really there would misplace the body
                    // check below rather than fail it.
                    Ok(0) => break,
                    Ok(n) => leftover += n,
                    Err(e) => panic!("draining the duplex failed after {leftover} bytes: {e}"),
                }
            }
            Run {
                cap,
                outcome: Outcome::Body {
                    content_length: cl,
                    body,
                    leftover,
                },
                head_bytes,
                overflow,
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the read path did not finish within {DRIVE_TIMEOUT:?} on a stream that had \
                 already reached EOF (BODY_READ_TIMEOUT is {BODY_READ_TIMEOUT:?})"
            )
        })
    })
}

/// Drive one input under every cap, asserting the invariants that make the
/// reader safe. Returns the first violated invariant, with the offending
/// input in the message, so the caller can persist it before panicking.
fn drive(rt: &Runtime, input: &[u8]) -> Result<Vec<Run>, String> {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let mut runs = Vec::with_capacity(CAPS.len());
    for &cap in &CAPS {
        let run = run_cap(rt, input, cap);
        check_invariants(input, cap, &run)?;
        runs.push(run);
    }
    Ok(runs)
}

fn check_invariants(input: &[u8], cap: usize, run: &Run) -> Result<(), String> {
    if !run.overflow && run.head_bytes > MAX_HEADER_BYTES {
        return Err(format!(
            "a {}-byte head (cap {cap}) reported no overflow; MAX_HEADER_BYTES is \
             {MAX_HEADER_BYTES}, so the header block is unbounded",
            run.head_bytes
        ));
    }
    let Outcome::Body {
        content_length,
        body,
        leftover,
    } = &run.outcome
    else {
        return Ok(());
    };
    match body {
        None => {
            if *content_length != 0 {
                return Err(format!(
                    "Content-Length {content_length} (cap {cap}) skipped the read"
                ));
            }
            Ok(())
        }
        Some(Ok(buf)) => {
            if buf.len() != *content_length {
                return Err(format!(
                    "Content-Length {content_length} (cap {cap}) returned {} bytes",
                    buf.len()
                ));
            }
            if *content_length > cap {
                return Err(format!(
                    "Content-Length {content_length} is over the cap {cap} yet a body was read"
                ));
            }
            // The stream delivers the request's bytes in order, so the body is
            // the run of input bytes immediately before the {leftover} bytes
            // still unread. Locating it this way pins the head/body split
            // without the harness re-deriving where the head read stopped.
            let Some(end) = input.len().checked_sub(*leftover) else {
                return Err(format!(
                    "Content-Length {content_length} (cap {cap}) left {leftover} bytes readable \
                     from a {}-byte request",
                    input.len()
                ));
            };
            let Some(start) = end.checked_sub(buf.len()) else {
                return Err(format!(
                    "Content-Length {content_length} (cap {cap}) returned {} bytes plus \
                     {leftover} readable from a {}-byte request",
                    buf.len(),
                    input.len()
                ));
            };
            if buf != &input[start..end] {
                return Err(format!(
                    "Content-Length {content_length} (cap {cap}) returned bytes other than the \
                     {}-byte run at input[{start}..{end}]",
                    buf.len()
                ));
            }
            Ok(())
        }
        Some(Err(ReadBodyError::TooLarge)) => {
            if *content_length <= cap {
                return Err(format!(
                    "Content-Length {content_length} fits the cap {cap} yet was rejected as \
                     TooLarge"
                ));
            }
            Ok(())
        }
        Some(Err(ReadBodyError::ReadFailed)) => {
            if *content_length == 0 {
                return Err(
                    "a zero Content-Length reported ReadFailed instead of an empty body"
                        .to_string(),
                );
            }
            Ok(())
        }
        Some(Err(ReadBodyError::TimedOut)) => Err(format!(
            "Content-Length {content_length} (cap {cap}) timed out on a stream that had reached \
             EOF"
        )),
    }
}

/// Escape an input for a panic message.
fn preview(bytes: &[u8]) -> String {
    const SHOWN: usize = 160;
    let mut out = String::new();
    for b in bytes.iter().take(SHOWN) {
        match b {
            b'\r' => out.push_str("\\r"),
            b'\n' => out.push_str("\\n"),
            0x20..=0x7e => out.push(*b as char),
            other => out.push_str(&format!("\\x{other:02x}")),
        }
    }
    if bytes.len() > SHOWN {
        out.push_str("...");
    }
    out
}

fn artifact_path(case: usize) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".tmp")
        .join("fuzz-artifacts")
        .join(format!("http-request-{}-{case}.bin", std::process::id()))
}

fn fail(input: &[u8], case: usize, finding: &str) -> ! {
    let path = artifact_path(case);
    let written = std::fs::create_dir_all(path.parent().expect("artifact parent"))
        .and_then(|()| std::fs::write(&path, input));
    let note = match &written {
        Ok(()) => format!("written to {}", path.display()),
        Err(e) => format!("could not write {}: {e}", path.display()),
    };
    panic!(
        "{finding}\ninput ({} bytes): {}\n{note}",
        input.len(),
        preview(input)
    )
}

/// Length values a raw byte mutation rarely produces but the parser must
/// survive: overflow, sign, padding, NUL, hex, exponent.
fn length_token(rng: &mut StdRng) -> &'static [u8] {
    match rng.random_range(0..11u8) {
        0 => b"0",
        1 => b"-1",
        2 => b"+6",
        3 => b" 6 ",
        4 => b"18446744073709551616",
        5 => b"99999999999999999999999999",
        6 => b"6\x00",
        7 => b"0x10",
        8 => b"06",
        9 => b"1e3",
        _ => b"",
    }
}

fn mutate(input: &mut Vec<u8>, rng: &mut StdRng) {
    if input.is_empty() {
        input.extend_from_slice(b"POST /api/meshes HTTP/1.1\r\n");
    }
    let len = input.len();
    match rng.random_range(0..11u8) {
        0 => {
            let i = rng.random_range(0..len);
            input[i] ^= 1 << rng.random_range(0..8);
        }
        1 => {
            let i = rng.random_range(0..len);
            input[i] = if rng.random_bool(0.5) { 0x00 } else { 0xff };
        }
        2 => {
            let i = rng.random_range(0..len);
            input.remove(i);
        }
        3 => {
            let i = rng.random_range(0..=len);
            input.insert(i, rng.random());
        }
        // Duplicate a slice: the classic way a body grows past its advertised
        // length.
        4 => {
            let start = rng.random_range(0..len);
            let end = start + rng.random_range(1..=(len - start));
            let chunk = input[start..end].to_vec();
            input.extend_from_slice(&chunk);
        }
        5 => {
            let keep = rng.random_range(0..=len);
            input.truncate(keep);
        }
        6 => input.extend_from_slice(b"\r\n\r\n"),
        7 => {
            input.extend_from_slice(b"Content-Length: ");
            input.extend_from_slice(length_token(rng));
            input.extend_from_slice(b"\r\n\r\n");
        }
        // A body with no header describing it.
        8 => input.extend_from_slice(b"\r\n\r\nabc123"),
        9 => input.extend_from_slice(b"Content-Length: 6\r\n\r\n"),
        _ => {
            let i = rng.random_range(0..len);
            input[i] = rng.random();
        }
    }
    input.truncate(MAX_INPUT_BYTES);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_matches_pinned_outcomes() {
        let rt = Runtime::new().expect("tokio runtime");
        for seed in SEEDS {
            let input = std::fs::read(corpus_dir().join(seed.file))
                .unwrap_or_else(|e| panic!("corpus seed {}: {e}", seed.file));
            let runs = drive(&rt, &input)
                .unwrap_or_else(|finding| fail(&input, 0, &format!("{}: {finding}", seed.file)));
            let run = runs
                .iter()
                .find(|run| run.cap == seed.cap)
                .unwrap_or_else(|| panic!("{}: cap {} was not driven", seed.file, seed.cap));
            assert_eq!(
                run.observed(),
                seed.expected.as_observed(),
                "{} at cap {}",
                seed.file,
                seed.cap
            );
        }
    }

    #[test]
    fn corpus_files_all_have_a_pinned_outcome() {
        let pinned: std::collections::BTreeSet<&str> = SEEDS.iter().map(|seed| seed.file).collect();
        let corpus = load_corpus();
        let unpinned: Vec<&str> = corpus
            .iter()
            .map(|(name, _)| name.as_str())
            .filter(|name| !pinned.contains(name))
            .collect();
        assert!(
            unpinned.is_empty(),
            "corpus file(s) with no pinned outcome in SEEDS: {unpinned:?}"
        );
    }

    #[test]
    fn header_overflow_is_rejected_before_any_body_read() {
        let rt = Runtime::new().expect("tokio runtime");
        let input = header_overflow_seed();
        assert!(
            input.len() > MAX_HEADER_BYTES,
            "the overflow seed must exceed MAX_HEADER_BYTES"
        );
        for run in drive(&rt, &input).expect("the overflow seed must hold its invariants") {
            assert_eq!(run.observed(), Observed::HeadOverflow, "at cap {}", run.cap);
        }
    }

    #[test]
    fn mutated_inputs_preserve_read_invariants() {
        let rt = Runtime::new().expect("tokio runtime");
        let cases = cases_from_env();
        let seed = seed_from_env();
        let mut corpus: Vec<Vec<u8>> = load_corpus().into_iter().map(|(_, bytes)| bytes).collect();
        corpus.push(header_overflow_seed());
        let mut rng = StdRng::seed_from_u64(seed);
        for case in 0..cases {
            let mut input = corpus[rng.random_range(0..corpus.len())].clone();
            for _ in 0..rng.random_range(1..4) {
                mutate(&mut input, &mut rng);
            }
            if let Err(finding) = drive(&rt, &input) {
                fail(&input, case, &finding);
            }
        }
        eprintln!(
            "http::fuzz: {cases} mutated inputs over {} seeds (BUILDMESH_FUZZ_SEED={seed:#x})",
            corpus.len()
        );
    }
}
