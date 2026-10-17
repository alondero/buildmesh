//! Fresh-process driver for scripts/bench-git-status.mjs. Fixture setup is
//! outside this process so its allocations do not contaminate query peaks.
use super::super::git::{get_git_status_blocking, get_git_summary_blocking, status_metrics};
use std::sync::{Arc, Barrier};
use std::time::Instant;

#[cfg(windows)]
fn memory() -> serde_json::Value {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        faults: u32,
        peak_working_set: usize,
        working_set: usize,
        peak_paged_pool: usize,
        paged_pool: usize,
        peak_nonpaged_pool: usize,
        nonpaged_pool: usize,
        pagefile: usize,
        peak_pagefile: usize,
        private: usize,
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(process: isize, counters: *mut Counters, size: u32) -> i32;
    }
    let mut counters = Counters::default();
    let size = std::mem::size_of::<Counters>() as u32;
    counters.cb = size;
    // -1 is the current-process pseudo handle. The struct includes the EX
    // private-usage field; PeakPagefileUsage is peak committed private bytes.
    assert_ne!(unsafe { GetProcessMemoryInfo(-1, &mut counters, size) }, 0);
    serde_json::json!({"private_bytes": counters.private,
        "peak_private_bytes": counters.peak_pagefile,
        "peak_working_set_bytes": counters.peak_working_set})
}

#[cfg(not(windows))]
fn memory() -> serde_json::Value {
    serde_json::Value::Null
}

#[test]
#[ignore = "run with scripts/bench-git-status.mjs (fresh process per fixture)"]
fn recurring_status() {
    let paths: Vec<String> =
        serde_json::from_str(&std::env::var("BUILDMESH_STATUS_BENCH_PATHS").unwrap()).unwrap();
    let iterations: usize = std::env::var("BUILDMESH_STATUS_BENCH_READS")
        .unwrap()
        .parse()
        .unwrap();
    let overlap = std::env::var("BUILDMESH_STATUS_BENCH_SUMMARY").unwrap() == "1";
    let before = memory();
    let start = Instant::now();
    let workers = paths.len() * if overlap { 2 } else { 1 };
    let barrier = Arc::new(Barrier::new(workers));
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for path in &paths {
            for summary in 0..if overlap { 2 } else { 1 } {
                let barrier = barrier.clone();
                handles.push(scope.spawn(move || {
                    let mut times = Vec::new();
                    let mut rows = 0;
                    let mut additions = 0;
                    let mut deletions = 0;
                    status_metrics::take();
                    for _ in 0..iterations {
                        barrier.wait();
                        let start = Instant::now();
                        if summary == 1 {
                            rows = get_git_summary_blocking(path.clone()).unwrap().total;
                        } else {
                            let result = get_git_status_blocking(path.clone()).unwrap();
                            rows = result.len();
                            additions = result.iter().map(|row| row.additions).sum();
                            deletions = result.iter().map(|row| row.deletions).sum();
                        }
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    serde_json::json!({"summary": summary == 1, "ms": times,
                        "rows": rows, "additions": additions, "deletions": deletions,
                        "counts_status_diff_patch": status_metrics::take()})
                }));
            }
        }
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    println!(
        "STATUS_BENCH {}",
        serde_json::json!({"before": before, "after": memory(),
        "wall_ms": start.elapsed().as_secs_f64() * 1000.0, "workers": results})
    );
}
