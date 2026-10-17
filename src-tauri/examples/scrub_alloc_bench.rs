//! Allocation and latency benchmark for the synchronous secret scrubber (#2426).
//!
//! Redaction runs on the thread that emits the log line, so every full-string
//! copy it makes is latency an agent's terminal or a git refresh pays. This
//! harness counts `alloc`/`realloc` calls and requested bytes for each input
//! shape through the real production functions — no copied redaction
//! implementation, no shortcut past the regexes.
//!
//! ```text
//! cd src-tauri
//! cargo run --release --example scrub_alloc_bench
//! ```
//!
//! Setup (building each input, compiling the regexes, warming the call) happens
//! outside the counters, so the numbers describe the steady state a running app
//! sees. Compare runs against each other; absolute numbers move with the
//! machine, the shape of the table is the contract.
//!
//! The counting allocator is process-wide, so this lives in its own binary
//! rather than in a test: a test process allocates for every other test running
//! beside it, and those numbers would be noise.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use buildmesh_lib::secret_scrubber::SecretScrubber;

/// Counts only while [`COUNTING`] is set, so the harness's own formatting and
/// setup never land in the totals.
struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static REALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

fn tally(realloc: bool, size: usize) {
    if !COUNTING.load(Ordering::Relaxed) {
        return;
    }
    if realloc {
        REALLOCS.fetch_add(1, Ordering::Relaxed);
    } else {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
    BYTES.fetch_add(size as u64, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        tally(false, layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        tally(false, layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        tally(true, new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// One benchmark input: the label, the exact bytes, and how many times to run
/// it. Short lines get enough iterations for a stable allocation count; a
/// 64 KiB line does not need ten thousand passes to be representative.
struct Case {
    label: &'static str,
    line: String,
    iterations: u32,
}

/// 70 bytes, the ordinary clean diagnostic line from #2426.
const CLEAN_PROSE: &str =
    "2026-10-16T16:30:00Z DEBUG buildmesh: refreshed git status for node 42\n";

/// 77 bytes: the same line carrying a JSON payload the writer parses and finds
/// clean.
const CLEAN_JSON: &str =
    "2026-10-16T16:30:00Z DEBUG buildmesh: result={\"node_id\":42,\"state\":\"running\"}\n";

const SECRET_PROSE: &str = "2026-10-16T16:30:00Z ERROR buildmesh: provider rejected \
     sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB s=42\n";

/// A secret under a secret-named key, inside an escaped JSON string inside a
/// JSON object: the nested/encoded shape, which costs a parse per embed depth.
const NESTED_JSON: &str = "2026-10-16T16:30:00Z DEBUG buildmesh: \
     {\"message\":\"{\\\"api_keys\\\":[\\\"plain-secret-no-shape\\\"]}\",\"author\":\"octocat\",\"count\":42}\n";

fn cases() -> Vec<Case> {
    let short = 10_000u32;
    let long = 200u32;
    vec![
        Case {
            label: "clean prose (70 B)",
            line: CLEAN_PROSE.to_string(),
            iterations: short,
        },
        Case {
            label: "clean JSON (77 B)",
            line: CLEAN_JSON.to_string(),
            iterations: short,
        },
        Case {
            label: "secret-bearing prose",
            line: SECRET_PROSE.to_string(),
            iterations: short,
        },
        Case {
            label: "nested/encoded JSON",
            line: NESTED_JSON.to_string(),
            iterations: short,
        },
        // Unbalanced braces: every `{` is a JSON candidate with no partner.
        Case {
            label: "64 KiB unbalanced braces",
            line: "{".repeat(64 * 1024),
            iterations: long,
        },
        // Balanced but invalid JSON: each candidate spans the rest of the line,
        // so this is the shape the per-pass parse budget exists to bound.
        Case {
            label: "64 KiB balanced non-JSON braces",
            line: format!("{}{}", "{".repeat(8 * 1024), "}".repeat(8 * 1024)),
            iterations: long,
        },
    ]
}

/// Allocations and requested bytes for one pass over `line`.
fn measure<F: FnMut()>(iterations: u32, mut body: F) -> (u64, u64) {
    COUNTING.store(true, Ordering::Relaxed);
    for _ in 0..iterations {
        body();
    }
    COUNTING.store(false, Ordering::Relaxed);
    let allocs = ALLOCS.swap(0, Ordering::Relaxed) + REALLOCS.swap(0, Ordering::Relaxed);
    (allocs, BYTES.swap(0, Ordering::Relaxed))
}

/// Wall-clock nanoseconds per call for one pass over `line`.
fn timed<F: FnMut()>(iterations: u32, mut body: F) -> f64 {
    let start = Instant::now();
    for _ in 0..iterations {
        body();
    }
    start.elapsed().as_nanos() as f64 / f64::from(iterations)
}

fn main() {
    println!("# secret scrubber allocation benchmark (issue #2426)");
    println!();
    println!("For each input: the String entry point every caller used before, then the");
    println!("borrowed entry point the log writer calls now.");
    println!();
    println!(
        "{:<30} {:>13} {:>13} {:>13} {:>13} {:>11} {:>11}",
        "case",
        "allocs String",
        "allocs borrowed",
        "bytes String",
        "bytes borrowed",
        "ns String",
        "ns borrowed"
    );
    println!("{}", "-".repeat(115));

    for case in cases() {
        // Warm up outside the counters: lazy regex compilation, first-touch
        // page faults and the input's own String are not what is measured.
        for _ in 0..100 {
            let _ = SecretScrubber::scrub_for_persistence(&case.line);
            let _ = SecretScrubber::scrub_for_persistence_borrowed(&case.line);
            let _ = SecretScrubber::scrub(&case.line);
        }

        // The `String` entry point is the one every caller used before the
        // refactor, so its column is what a before/after comparison is made
        // against. The borrowed column is what the log writer now calls.
        let (string_allocs, string_bytes) = measure(case.iterations, || {
            let scrubbed = SecretScrubber::scrub_for_persistence(&case.line);
            std::hint::black_box(&scrubbed);
        });
        let (borrowed_allocs, borrowed_bytes) = measure(case.iterations, || {
            let scrubbed = SecretScrubber::scrub_for_persistence_borrowed(&case.line);
            std::hint::black_box(&scrubbed);
        });
        let string_nanos = timed(case.iterations, || {
            let scrubbed = SecretScrubber::scrub_for_persistence(&case.line);
            std::hint::black_box(&scrubbed);
        });
        let borrowed_nanos = timed(case.iterations, || {
            let scrubbed = SecretScrubber::scrub_for_persistence_borrowed(&case.line);
            std::hint::black_box(&scrubbed);
        });

        println!(
            "{:<30} {:>13.2} {:>13.2} {:>13} {:>13} {:>11.0} {:>11.0}",
            case.label,
            string_allocs as f64 / f64::from(case.iterations),
            borrowed_allocs as f64 / f64::from(case.iterations),
            string_bytes / u64::from(case.iterations),
            borrowed_bytes / u64::from(case.iterations),
            string_nanos,
            borrowed_nanos,
        );
    }
}
