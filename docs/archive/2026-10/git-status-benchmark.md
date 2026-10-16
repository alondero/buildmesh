# Git status benchmark, 2026-10-16

Historical measurement for issue #2425. Current ownership and freshness live
in [Git query cache](../../development/git-query-cache.md#backend-status-line-counts).

Baseline: `1ffe0adda661f49694ec433aaa3905c7c040d7b9`, with only test counters
and the ignored benchmark driver added. Comparison: the same real blocking
commands with streaming simple-delta counts and the clean-read diff guard.
Both executables were retained by the worktree-stamped Rust runner. This
Windows x64 run used unoptimized debug tests on AMD Ryzen 9 3900X 12-Core Processor            .
These are synthetic fixture measurements, not production RSS or release latency.

## Method

Each baseline/current pair reads the same temporary repositories. Pair order
alternates between cases. Fixture generation happens outside the fresh worker
process. Each worker reads five times; the latency below is the median after
excluding its first read. Multi-worktree medians pool status workers only.
Peak private commit is Windows PROCESS_MEMORY_COUNTERS_EX PeakPagefileUsage
for the whole worker process, including cold reads, libgit2 C allocations,
and any concurrent summary workers. Memory is not a Rust-allocator estimate.

[Machine-readable summary](git-status-benchmark.json) records timings, peak
private bytes and status/diff/patch invocation counts. The full per-read JSON
and binaries remain in the ignored task artifacts. Reproduce with
`node scripts/bench-git-status.mjs .tmp/results.json`; optional executable
arguments let the same generated fixtures run against retained current and
baseline binaries. Build mobile assets first as documented in the owner page.

## Results

| Case | Before steady ms | After steady ms | Before peak MiB | After peak MiB |
|---|---:|---:|---:|---:|
| clean | 9.74 | 7.74 | 3.06 | 3.07 |
| small | 12.31 | 11.86 | 3.07 | 3.06 |
| generated-1MiB | 65.53 | 35.29 | 6.78 | 4.15 |
| generated-10MiB | 439.18 | 166.95 | 39.74 | 13.20 |
| generated-10MiB-interior | 371.07 | 543.01 | 39.73 | 39.75 |
| generated-100MiB | 3463.77 | 1355.47 | 355.61 | 107.50 |
| untracked-100MiB | 1857.40 | 1344.72 | 228.38 | 3.12 |
| many-2000 | 2444.90 | 2475.66 | 5.28 | 5.63 |
| many-status-summary-overlap | 2378.70 | 2480.14 | 6.59 | 6.27 |
| many-generated-100x1MiB | 4227.85 | 1485.09 | 7.00 | 4.18 |
| four-worktrees-10MiB | 152.97 | 127.55 | 78.67 | 4.41 |
| four-worktrees-status-summary-overlap | 179.65 | 154.93 | 79.18 | 5.14 |

The 100 MiB append case reduces peak private commit by about 70%; the untracked
case reduces it by about 99%. The 100-file generated case reduces median
latency by about 65% and peak private commit by about 40%. The 2,000 small-file
case retains native patches and stays within about 1.3% in this paired run;
the ordinary small edit also stays on the native path. Clean reads go from
five line-diff walks to zero. Eligible large cases create no patches.

The interior-edit control has the same native patch memory and higher latency:
the prefix probe loads the original blob before discovering that it must use
the native patch. This optimization benefits simple deltas; it does not remove
patch storage from arbitrary interior edits. In this run that control costs
543 ms versus 371 ms. Attributes, filters, special modes and partial-line
prefix edits retain the native calculation as well.

## Overlap and freshness decision

Status/summary overlap repeats a status walk, but summary performs no line diff
or patch work. The initial 2,000-file overlap measured summary at about 15 ms
against status at about 2.4 s. No backend request sharing was added: the extra
walk is small relative to line statistics, and summary stays independent of
that work. Every call reopens the requested path; there is no backend cached
failure, clean observation or mutation invalidation policy to maintain.

## Correctness and platform evidence

The production-command regressions compare native patch counts and literal
rows, including stage plus workdir changes, moves, project projections,
unborn HEAD, binary/EOF boundaries, attributes and legacy crlf conversion.
The existing Windows ignored-junction regression passed. A live mapped WSL
path (`/mnt/f/...`) returned one untracked row, exactly 20,000 additions and
zero deletions over three overlapping status/summary rounds, without patches.
A Linux-owned UNC fixture was rejected by libgit2's ownership validation before
line statistics; live UNC success is not established by this run.
