//! Tests for `agent::provider::adapters::codex`.
//!
//! Split out of `src-tauri/src/agent/provider/adapters/codex.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test agent::provider::adapters::codex::tests

use super::*;
use tempfile::TempDir;

/// Regression guard: no read probe in this module may spawn a child
/// with a bare `.output()`.
///
/// Settings can request the Codex install for Windows and the foreign
/// runtime through both `available_providers` and pairing statuses. The
/// cache collapses repeated requests, but every actual probe still needs
/// a timeout: an unbounded child blocks forever if it never exits — a
/// wedged `wsl.exe` on a paused VM, a `.cmd`
/// shim stuck on a console handle. That pinned a blocking-pool thread
/// for the life of the process with no way for the user to recover, so
/// the tab sat on "loading" indefinitely.
///
/// This is a source-shape assertion rather than a live hung-probe test
/// because the failure needs a genuinely un-exitable child in a
/// specific runtime, which can't be fabricated portably. `run_command_with_timeout`
/// itself is covered in `process_util` (kill-on-deadline, early-exit,
/// spawn-failure); what's pinned here is that this module keeps routing
/// through it. Mirrors `process_util::git_command_disables_interactive_prompts`,
/// which guards the same class of regression for the git shell-out.
#[test]
fn no_codex_read_probe_spawns_without_a_timeout() {
    // `codex.rs` is production code in full — its tests live in this sibling
    // file since #2451 — so the whole file is the subject of the guard. It
    // used to `split_once("#[cfg(test)]\nmod tests {")` and `.expect(...)`
    // that marker, which made the file's own layout a load-bearing fixture:
    // the split became the assertion's premise rather than its subject.
    let production = include_str!("codex.rs");
    // Strip comment lines first: this file's own doc comments name
    // `.output()` when explaining the bug, and prose about the hazard
    // must not read as an instance of it.
    let code: String = production
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//") && !trimmed.starts_with('*') && !trimmed.starts_with("/*")
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains(".output()"),
        "every Codex probe must use process_util::run_command_with_timeout — \
             a bare `.output()` can block forever and strands the Settings → \
             Providers tab on a spinner with no recovery"
    );
}

fn test_install(version: &str) -> CodexInstall {
    CodexInstall {
        executable: "codex-test".into(),
        version: version.into(),
        runtime_identity: "test-runtime".into(),
        codex_home: "/tmp/codex-test".into(),
        wsl_distro: None,
    }
}

fn wait_for_probe_release(release: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
    let (lock, condition) = &**release;
    let released = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _released = condition
        .wait_while(released, |released| !*released)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
}

fn release_probes(release: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
    let (lock, condition) = &**release;
    *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    condition.notify_all();
}

#[test]
fn install_resolution_is_single_flight_and_refreshes_after_ttl() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    let cache = Arc::new(CodexInstallCache::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let (probe_started_tx, probe_started_rx) = mpsc::channel();
    let first_cache = Arc::clone(&cache);
    let first_calls = Arc::clone(&calls);
    let first_release = Arc::clone(&release);
    let first = thread::spawn(move || {
        first_cache.discover(EnvType::Wsl, || {
            first_calls.fetch_add(1, Ordering::SeqCst);
            probe_started_tx.send(()).unwrap();
            wait_for_probe_release(&first_release);
            Ok(test_install("0.144.0"))
        })
    });
    probe_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first caller starts the runtime probe");

    let (second_calling_tx, second_calling_rx) = mpsc::sync_channel(0);
    let second_cache = Arc::clone(&cache);
    let second_calls = Arc::clone(&calls);
    let second = thread::spawn(move || {
        second_calling_tx.send(()).unwrap();
        second_cache.discover(EnvType::Wsl, || {
            second_calls.fetch_add(1, Ordering::SeqCst);
            Ok(test_install("0.145.0"))
        })
    });
    second_calling_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("second caller reaches the same runtime cache");
    release_probes(&release);

    assert_eq!(first.join().unwrap().unwrap().version, "0.144.0");
    assert_eq!(second.join().unwrap().unwrap().version, "0.144.0");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let resolved_at = cache
        .entries
        .lock()
        .unwrap()
        .get(runtime_identity(EnvType::Wsl))
        .unwrap()
        .cell
        .get()
        .unwrap()
        .resolved_at;
    let just_before_expiry = resolved_at + CODEX_INSTALL_CACHE_TTL - Duration::from_nanos(1);
    let still_cached = cache
        .discover_at(EnvType::Wsl, just_before_expiry, || {
            panic!("a fresh install result must be reused until its TTL expires")
        })
        .unwrap();
    assert_eq!(still_cached.version, "0.144.0");

    let refreshed = cache
        .discover_at(EnvType::Wsl, resolved_at + CODEX_INSTALL_CACHE_TTL, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(test_install("0.145.0"))
        })
        .unwrap();
    assert_eq!(refreshed.version, "0.145.0");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_caller_whose_clock_read_precedes_the_resolution_reuses_it() {
    // `discover` reads the clock before it takes the cache lock, so a
    // concurrent caller's resolution can land with a `resolved_at` later
    // than this caller's `now`. That entry is brand new, not stale;
    // treating it as stale replaced the slot and ran a duplicate probe.
    let cache = CodexInstallCache::default();
    cache
        .discover(EnvType::Wsl, || Ok(test_install("0.144.0")))
        .unwrap();
    let resolved_at = cache
        .entries
        .lock()
        .unwrap()
        .get(runtime_identity(EnvType::Wsl))
        .unwrap()
        .cell
        .get()
        .unwrap()
        .resolved_at;

    // Milliseconds, not nanoseconds: under load the gap between the clock
    // read and the lock is that wide, and Windows' `Instant` rounds a
    // sub-tick difference to zero, which would hide the bug.
    let earlier_clock_read = resolved_at - Duration::from_millis(1);
    let reused = cache
        .discover_at(EnvType::Wsl, earlier_clock_read, || {
            panic!("a resolution newer than the caller's clock read must be reused")
        })
        .unwrap();
    assert_eq!(reused.version, "0.144.0");
}

#[test]
fn separate_runtime_probes_can_run_at_the_same_time() {
    use std::sync::mpsc;
    use std::thread;

    let cache = Arc::new(CodexInstallCache::default());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let (probe_started_tx, probe_started_rx) = mpsc::channel();
    let mut probes = Vec::new();
    for (env_type, version) in [
        (EnvType::Windows, "0.144.0"),
        (EnvType::Wsl, "0.145.0"),
        (EnvType::WindowsInterop, "0.146.0"),
    ] {
        let cache = Arc::clone(&cache);
        let release = Arc::clone(&release);
        let probe_started_tx = probe_started_tx.clone();
        probes.push(thread::spawn(move || {
            cache.discover(env_type, || {
                probe_started_tx.send(env_type).unwrap();
                wait_for_probe_release(&release);
                Ok(test_install(version))
            })
        }));
    }
    drop(probe_started_tx);
    let started = (0..3)
        .filter_map(|_| probe_started_rx.recv_timeout(Duration::from_secs(2)).ok())
        .collect::<Vec<_>>();
    release_probes(&release);
    for probe in probes {
        probe.join().unwrap().unwrap();
    }
    assert_eq!(
        started.len(),
        3,
        "each runtime must start before either probe completes"
    );
}

#[test]
fn fresh_install_resolution_replaces_the_cached_version() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cache = CodexInstallCache::default();
    let calls = AtomicUsize::new(0);
    let initial = cache
        .discover(EnvType::Windows, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(test_install("0.144.0"))
        })
        .unwrap();
    assert_eq!(initial.version, "0.144.0");

    let verified = cache
        .discover_fresh(EnvType::Windows, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(test_install("0.145.0"))
        })
        .unwrap();
    assert_eq!(verified.version, "0.145.0");

    let next_read = cache
        .discover(EnvType::Windows, || {
            panic!("fresh verification should have replaced the cached install")
        })
        .unwrap();
    assert_eq!(next_read.version, "0.145.0");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_install_resolution_is_retried_on_the_next_read() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cache = CodexInstallCache::default();
    let calls = AtomicUsize::new(0);
    let failed = cache.discover(EnvType::Windows, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Err("Codex is unavailable".into())
    });
    assert_eq!(failed.unwrap_err(), "Codex is unavailable");

    let recovered = cache
        .discover(EnvType::Windows, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(test_install("0.145.0"))
        })
        .unwrap();
    assert_eq!(recovered.version, "0.145.0");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Issue #1948: the cold-vs-warm bit in the provider-menu timing log
/// must agree with what `discover` would do — report cached only when a
/// fresh entry exists, without probing. A panicking probe proves the
/// warm path performs no process work.
#[test]
fn install_cache_freshness_reports_cold_and_warm_without_probing() {
    let cache = CodexInstallCache::default();
    assert!(!cache.is_fresh(EnvType::Windows), "an empty cache is cold");

    cache
        .discover(EnvType::Windows, || Ok(test_install("0.144.0")))
        .unwrap();
    assert!(
        cache.is_fresh(EnvType::Windows),
        "a resolved install is warm"
    );
    assert!(!cache.is_fresh(EnvType::Wsl), "freshness is per-runtime");

    // The warm read must reuse the entry: a probe here would panic.
    let warm = cache
        .discover(EnvType::Windows, || {
            panic!("a fresh install result must be reused without probing")
        })
        .unwrap();
    assert_eq!(warm.version, "0.144.0");

    let resolved_at = cache
        .entries
        .lock()
        .unwrap()
        .get(runtime_identity(EnvType::Windows))
        .unwrap()
        .cell
        .get()
        .unwrap()
        .resolved_at;
    assert!(cache.is_fresh_at(
        EnvType::Windows,
        resolved_at + CODEX_INSTALL_CACHE_TTL - Duration::from_nanos(1)
    ));
    assert!(
        !cache.is_fresh_at(EnvType::Windows, resolved_at + CODEX_INSTALL_CACHE_TTL),
        "an expired entry is cold again"
    );

    // Failures are discarded, never served warm: the next read probes.
    let failures = CodexInstallCache::default();
    let _ = failures.discover(EnvType::Windows, || {
        Err::<CodexInstall, String>("Codex is unavailable".into())
    });
    assert!(
        !failures.is_fresh(EnvType::Windows),
        "a failed probe leaves no warm entry"
    );
}

/// The probe bounds must stay generous enough for a cold WSL distro
/// start (seconds) but finite. A zero/absurd value would make the
/// timeout above a self-inflicted outage rather than a backstop.
#[test]
fn codex_probe_timeouts_are_finite_and_non_trivial() {
    assert!(CODEX_PROBE_TIMEOUT >= std::time::Duration::from_secs(5));
    assert!(CODEX_LOOKUP_TIMEOUT >= std::time::Duration::from_secs(5));
    // Lookups are plain host/guest reads, so they stay tighter than the
    // CLI capability probes (which may go through PowerShell).
    assert!(CODEX_LOOKUP_TIMEOUT < CODEX_PROBE_TIMEOUT);
}

/// How long a probe may wait for its siblings before the concurrency
/// assertions call the chain serial.
const PROBE_RENDEZVOUS: std::time::Duration = std::time::Duration::from_secs(2);

/// An all-participants rendezvous, so the concurrency assertions are
/// deterministic rather than a sleep-and-hope margin: every probe blocks
/// until all of its siblings have arrived, so they can only all return if
/// they were genuinely in flight together. A regression to the serial chain
/// strands the first probe until the bound fires, which turns into a failing
/// test instead of a hung one.
struct Rendezvous {
    arrived: Mutex<usize>,
    released: std::sync::Condvar,
    participants: usize,
}

impl Rendezvous {
    fn new(participants: usize) -> Self {
        Self {
            arrived: Mutex::new(0),
            released: std::sync::Condvar::new(),
            participants,
        }
    }

    fn wait(&self) -> Result<(), String> {
        let mut arrived = self.arrived.lock().unwrap_or_else(|p| p.into_inner());
        *arrived += 1;
        // Loop on the predicate, not on a single wait: a condvar may wake
        // spuriously, and passing on that wakeup would let a probe return
        // "overlapped" before its siblings had arrived - exactly the false
        // pass this rendezvous exists to prevent.
        loop {
            if *arrived >= self.participants {
                self.released.notify_all();
                return Ok(());
            }
            let (guard, timeout) = self
                .released
                .wait_timeout(arrived, PROBE_RENDEZVOUS)
                .unwrap_or_else(|p| p.into_inner());
            arrived = guard;
            if timeout.timed_out() && *arrived < self.participants {
                return Err(format!(
                        "probe never overlapped its {} siblings within {PROBE_RENDEZVOUS:?} - the chain ran serially",
                        self.participants - 1
                    ));
            }
        }
    }
}

/// Issue #1934: the three identity probes are independent, so they must be
/// issued concurrently. Serially they cost their sum — three `wsl.exe`
/// starts, ~340ms each warm and seconds on a cold distro — where the user
/// only ever waits for the slowest one.
///
/// This proves the joiner overlaps its probes. That
/// `discover_supported_install_uncached` routes through it is enforced by
/// the compiler, not here: the call site passes bare `probe_codex_*` items,
/// so dropping or reordering one is a type error.
#[test]
fn codex_identity_probes_are_issued_concurrently() {
    let rendezvous = Rendezvous::new(3);
    let (version, executable, home) = probe_codex_identity_concurrently(
        EnvType::Windows,
        None,
        |_, _| {
            rendezvous
                .wait()
                .map(|_| CodexVersion("0.158.0".to_string()))
        },
        |_, _| {
            rendezvous
                .wait()
                .map(|_| CodexExecutable("/usr/bin/codex".to_string()))
        },
        |_, _| {
            rendezvous
                .wait()
                .map(|_| CodexHome("/home/dev/.codex".to_string()))
        },
    )
    .expect("all three probes must rendezvous and succeed");
    assert_eq!(version.0, "0.158.0");
    assert_eq!(executable.0, "/usr/bin/codex");
    assert_eq!(home.0, "/home/dev/.codex");
}

/// The concurrent chain must not change which message a broken install
/// produces. A missing `codex` fails all three probes at once, and the
/// user has to keep seeing the version failure — the root cause — rather
/// than the location/home failures that only follow from it.
#[test]
fn codex_identity_error_precedence_keeps_the_serial_order() {
    assert_eq!(
        probe_codex_identity_concurrently(
            EnvType::Windows,
            None,
            |_, _| Err("Codex version check failed".to_string()),
            |_, _| Err("Codex executable identity is unavailable".to_string()),
            |_, _| Err("WSL Codex home identity is unavailable".to_string()),
        )
        .expect_err("every probe failed"),
        "Codex version check failed",
        "a broken install must report the version failure, not a downstream symptom"
    );
    assert_eq!(
        probe_codex_identity_concurrently(
            EnvType::Windows,
            None,
            |_, _| Ok(CodexVersion("0.158.0".to_string())),
            |_, _| Err("Codex executable identity is unavailable".to_string()),
            |_, _| Err("WSL Codex home identity is unavailable".to_string()),
        )
        .expect_err("location and home failed"),
        "Codex executable identity is unavailable",
        "the version probe passing must expose the location failure next"
    );
    assert_eq!(
        probe_codex_identity_concurrently(
            EnvType::Windows,
            None,
            |_, _| Ok(CodexVersion("0.158.0".to_string())),
            |_, _| Ok(CodexExecutable("/usr/bin/codex".to_string())),
            |_, _| Err("WSL Codex home identity is unavailable".to_string()),
        )
        .expect_err("only the home probe failed"),
        "WSL Codex home identity is unavailable"
    );
}

/// Issue #1934: the two `--help` capability probes are independent reads of
/// the same CLI, so they overlap too.
#[test]
fn proxy_cli_help_probes_are_issued_concurrently() {
    let rendezvous = Rendezvous::new(2);
    let (fresh, resume) = probe_proxy_cli_help_concurrently(
        EnvType::Windows,
        None,
        |_, _| {
            rendezvous
                .wait()
                .map(|_| FreshHelp("fresh help".to_string()))
        },
        |_, _| {
            rendezvous
                .wait()
                .map(|_| ResumeHelp("resume help".to_string()))
        },
    )
    .expect("both help probes must rendezvous and succeed");
    assert_eq!(fresh.0, "fresh help");
    assert_eq!(resume.0, "resume help");
}

/// The help pair keeps probe-order precedence, matching both the serial
/// chain and the `("fresh", ..)` order [`validate_proxy_cli_help`] reports
/// a missing flag in.
#[test]
fn proxy_cli_help_error_precedence_reports_fresh_first() {
    assert_eq!(
        probe_proxy_cli_help_concurrently(
            EnvType::Windows,
            None,
            |_, _| Err("Codex fresh capability check failed".to_string()),
            |_, _| Err("Codex resume capability check failed".to_string()),
        )
        .expect_err("both help probes failed"),
        "Codex fresh capability check failed"
    );
    assert_eq!(
        probe_proxy_cli_help_concurrently(
            EnvType::Windows,
            None,
            |_, _| Ok(FreshHelp("fresh help".to_string())),
            |_, _| Err("Codex resume capability check failed".to_string()),
        )
        .expect_err("only the resume probe failed"),
        "Codex resume capability check failed"
    );
}

/// Finding #1: a probe panic must reach the boundary above carrying the
/// message that named the probe. `join().unwrap()` replaces the payload
/// with `Any { .. }`, so the joiners re-raise with `resume_unwind` instead;
/// this pins that the payload survives.
#[test]
fn a_panicking_probe_keeps_its_message_at_the_join_boundary() {
    let boundary = std::panic::catch_unwind(|| {
        let _ = probe_codex_identity_concurrently(
            EnvType::Windows,
            None,
            |_, _| -> Result<CodexVersion, String> { panic!("WSL Codex location probe blew up") },
            |_, _| Ok(CodexExecutable("/usr/bin/codex".to_string())),
            |_, _| Ok(CodexHome("/home/dev/.codex".to_string())),
        );
    });
    let payload = boundary.expect_err("the panicking probe must re-raise on the joining thread");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|m| m.to_string()))
        .expect("panic payload must still be a string");
    assert_eq!(
        message, "WSL Codex location probe blew up",
        "the probe's own message must survive the join, not degrade to `Any {{ .. }}`"
    );
}

#[test]
fn multiline_automated_prefill_uses_pty_transport() {
    assert!(!CODEX.prefill_requires_pty("review the PR"));
    assert!(CODEX.prefill_requires_pty("review the diff\n+ added line"));
    assert!(CODEX.prefill_requires_pty("review the diff\r+ added line"));
    assert!(CODEX.prefill_requires_pty("- review this change"));
    assert!(CODEX.prefill_requires_pty("+ review this change"));
}

#[test]
fn initial_prompt_waits_for_the_codex_composer() {
    // Real frames captured from a live Codex 0.158 PTY. The model is only
    // ever a bare `loading` line on boot and a `GPT-6-Luna default · <dir>`
    // status footer once resolved — neither the old `model: <name>
    // /model to change` banner nor a non-"loading" model header is ever
    // rendered, so the previous gate never fired (run 261, node 4618).
    let boot = ">_ OpenAI Codex (v0.158.0)\nloading\n\
                    › Ask Codex to do anything\n? for shortcuts";
    let loaded = ">_ OpenAI Codex (v0.158.0)\nloading\n\
                      › Ask Codex to do anything\n? for shortcuts\nF:\\src\\nestlin\n\
                      GPT-6-Luna default · F:\\src\\nestlin\nxhigh · F:\\src\\nestlin";
    let resumed = "› Ask a follow-up question";
    let pre_composer = ">_ OpenAI Codex (v0.158.0)\nloading";
    assert!(!CODEX.ready_for_initial_prompt(""));
    assert!(!CODEX.ready_for_initial_prompt(pre_composer));
    assert!(CODEX.ready_for_initial_prompt(boot));
    assert!(CODEX.ready_for_initial_prompt(loaded));
    assert!(CODEX.ready_for_initial_prompt(resumed));
}

#[test]
fn stable_profile_identity_survives_endpoint_and_model_edits() {
    let before = stable_profile_name("codex", "minimax");
    let after = stable_profile_name("codex", "minimax");
    assert_eq!(before, after);
    assert!(before.starts_with("buildmesh_"));
    assert_ne!(before, stable_profile_name("codex", "another-provider"));
}

#[test]
fn codex_home_resolution_honours_explicit_and_default_locations() {
    let explicit = native_codex_home_from(
        Some(std::ffi::OsString::from("/custom/codex")),
        Some(std::ffi::OsString::from("/home/user")),
    )
    .unwrap();
    assert_eq!(explicit, std::path::PathBuf::from("/custom/codex"));
    let default =
        native_codex_home_from(None, Some(std::ffi::OsString::from("/home/user"))).unwrap();
    assert_eq!(default, std::path::PathBuf::from("/home/user/.codex"));
    assert!(native_codex_home_from(None, None).is_err());
}

#[test]
fn wsl_codex_home_uses_guest_default_without_host_override() {
    assert!(WSL_CODEX_HOME_SCRIPT.contains("${CODEX_HOME:-$HOME/.codex}"));
    assert!(!CODEX.wsl_passthrough_env().contains(&"CODEX_HOME"));
    assert_ne!(
        wsl_runtime_identity("Ubuntu", "/home/user/.codex"),
        wsl_runtime_identity("Debian", "/home/user/.codex")
    );
    assert_ne!(
        wsl_runtime_identity("Ubuntu", "/home/user/.codex"),
        wsl_runtime_identity("Ubuntu", "/custom/codex")
    );
}

/// Issue #1937 review: a successful WSL probe resolves
/// `wsl:{distro}:{codex_home}` where the home carries the guest
/// username. The logged value must keep the distro (which host was
/// slow) and drop the path — this is the value-level coverage for the
/// successful-probe arm of the derivation log (driving a real probe
/// here would need a live Codex install per runtime).
/// Both Windows locators answer with every match, and an npm Windows
/// prefix holds the runnable `.cmd` beside the extensionless POSIX shim.
/// Taking the whole answer put a CRLF inside the single-quoted PowerShell
/// literal this path is destined for, which made the resolved executable
/// unusable — so the parser must take one line, and the runnable one.
#[test]
fn windows_locator_answer_keeps_only_the_first_runnable_line() {
    let answer = "C:\\Users\\alond\\AppData\\Roaming\\npm\\codex.cmd\r\n\
                      C:\\Users\\alond\\AppData\\Roaming\\npm\\codex\r\n";
    assert_eq!(
        super::first_runnable_windows_path(answer),
        r"C:\Users\alond\AppData\Roaming\npm\codex.cmd",
        "the runnable shim must be chosen and the trailing shim dropped, or the \
             resolved executable carries a CRLF into a single-quoted literal"
    );
}

/// An answer with no runnable entry resolves to empty, which the caller
/// reports as an unusable install rather than passing on a bare stem.
#[test]
fn windows_locator_answer_without_a_runnable_entry_is_empty() {
    assert_eq!(
        super::first_runnable_windows_path("/usr/local/bin/codex\n/usr/bin/codex\n"),
        ""
    );
}

#[test]
fn log_safe_runtime_identity_omits_wsl_codex_home() {
    let wsl = CodexInstall {
        executable: "/home/alice/.local/bin/codex".into(),
        version: "1.2.3".into(),
        runtime_identity: wsl_runtime_identity("Ubuntu", "/home/alice/.codex"),
        codex_home: "/home/alice/.codex".into(),
        wsl_distro: Some("Ubuntu".into()),
    };
    assert_eq!(wsl.runtime_identity, "wsl:Ubuntu:/home/alice/.codex");
    let logged = log_safe_runtime_identity(&wsl);
    assert_eq!(logged, "wsl:Ubuntu");
    assert!(
        !logged.contains("alice"),
        "guest username must not reach logs: {logged}"
    );
    let native = CodexInstall {
        executable: "codex".into(),
        version: "1.2.3".into(),
        runtime_identity: "native-windows".into(),
        codex_home: String::new(),
        wsl_distro: None,
    };
    assert_eq!(log_safe_runtime_identity(&native), "native-windows");
}

#[cfg(windows)]
#[test]
#[ignore = "requires a local WSL distribution"]
fn wsl_profile_materializes_in_default_and_explicit_codex_home() {
    let distro = crate::env::detect_default_wsl_distro().expect("WSL distribution");
    let default_home = crate::process_util::command_no_window("wsl.exe")
        .args(["-d", &distro, "--exec", "sh", "-lc", WSL_CODEX_HOME_SCRIPT])
        .env_remove("CODEX_HOME")
        .output()
        .unwrap();
    assert!(default_home.status.success());
    let default_home = crate::env::parse_wsl_codex_home_output(&default_home.stdout)
        .expect("WSL Codex home marker")
        .to_string_lossy()
        .into_owned();
    assert!(!default_home.is_empty());
    let explicit_home = format!("/tmp/buildmesh-codex-profile-test-{}", std::process::id());

    for (index, home) in [default_home, explicit_home.clone()]
        .into_iter()
        .enumerate()
    {
        let profile = format!("buildmesh_wsl_contract_{}_{}", std::process::id(), index);
        let install = CodexInstall {
            executable: "/usr/bin/codex".into(),
            version: "test".into(),
            runtime_identity: wsl_runtime_identity(&distro, &home),
            codex_home: home.clone(),
            wsl_distro: Some(distro.clone()),
        };
        let expected = render_proxy_profile(&profile, "WSL contract", "https://example.invalid/v1");
        materialize_proxy_profile(
            EnvType::Wsl,
            &install,
            &profile,
            "WSL contract",
            "https://example.invalid/v1",
        )
        .unwrap();
        let output = crate::process_util::command_no_window("wsl.exe")
            .args([
                "-d",
                &distro,
                "--exec",
                "sh",
                "-c",
                "cat \"$1/$2.config.toml\"",
                "buildmesh-test",
                &home,
                &profile,
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        let _ = crate::process_util::command_no_window("wsl.exe")
            .args([
                "-d",
                &distro,
                "--exec",
                "sh",
                "-c",
                "rm -f \"$1/$2.config.toml\"",
                "buildmesh-test",
                &home,
                &profile,
            ])
            .status();
    }
    let _ = crate::process_util::command_no_window("wsl.exe")
        .args(["-d", &distro, "--exec", "rmdir", &explicit_home])
        .status();
}

#[test]
fn proxy_profile_is_exact_secret_free_and_toml_escaped() {
    let rendered = render_proxy_profile(
        "buildmesh_1234",
        "Provider \"quoted\"",
        "https://example.com/v1/\"quoted\"",
    );
    assert_eq!(
            rendered,
            "model_provider = \"buildmesh_1234\"\n\n[model_providers.buildmesh_1234]\nname = \"Buildmesh: Provider \\\"quoted\\\"\"\nbase_url = \"https://example.com/v1/\\\"quoted\\\"\"\nwire_api = \"responses\"\nenv_key = \"BUILDMESH_CODEX_PROVIDER_KEY\"\nrequires_openai_auth = false\n"
        );
    assert!(!rendered.contains("OPENAI_API_KEY"));
}

#[test]
fn supported_version_floor_is_strict() {
    assert_eq!(parse_version("codex-cli 0.144.0").unwrap().0, 0);
    let old = parse_version("codex-cli 0.143.9").unwrap();
    assert!((old.0, old.1, old.2) < MIN_PROXY_CODEX_VERSION);
    assert!(parse_version("custom build").is_none());
}

#[test]
fn proxy_cli_requires_profile_and_model_for_fresh_and_resume() {
    let complete = "Usage: codex [OPTIONS]\n  --profile <NAME>\n  --model <MODEL>";
    assert!(validate_proxy_cli_help(complete, complete).is_ok());
    let error = validate_proxy_cli_help(complete, "Usage: codex resume").unwrap_err();
    assert!(error.contains("resume"));
    assert!(error.contains("--profile"));
    assert!(error.contains("--model"));
}

/// Opt-in real-CLI profile-routing check. CI installs the exact version
/// named in `.github/workflows/build.yml` before selecting this test.
#[test]
#[ignore = "requires the pinned Codex CLI installed by workflow_dispatch"]
fn pinned_codex_cli_loads_profile_for_fresh_and_resume() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let cold_start = std::time::Instant::now();
    let mut install = discover_supported_install(EnvType::Windows).unwrap();
    assert_eq!(install.version, "0.147.0");
    assert!(cold_start.elapsed() < std::time::Duration::from_secs(5));

    let temp = TempDir::new().unwrap();
    install.codex_home = temp.path().to_string_lossy().into_owned();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request);
                let Some(header_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let length = text[..header_end]
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_string)
                    })
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + length {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(
                request.starts_with("POST /v1/responses HTTP/1.1"),
                "{request}"
            );
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer pinned-secret"));
            assert!(request.contains("\"model\":\"MiniMax-M3\""));
            let response_id = format!("resp_{}", index + 1);
            let message_id = format!("msg_{}", index + 1);
            let completed = serde_json::json!({
                "id": response_id,
                "object": "response",
                "created_at": 1_700_000_000,
                "status": "completed",
                "error": null,
                "incomplete_details": null,
                "instructions": null,
                "max_output_tokens": null,
                "model": "MiniMax-M3",
                "output": [{
                    "id": message_id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{"type":"output_text","text":"verified","annotations":[]}]
                }],
                "parallel_tool_calls": false,
                "previous_response_id": null,
                "reasoning": {"effort":"medium","summary":null},
                "store": false,
                "temperature": null,
                "text": {"format":{"type":"text"}},
                "tool_choice": "auto",
                "tools": [],
                "top_p": null,
                "truncation": "disabled",
                "usage": {"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2},
                "user": null,
                "metadata": {}
            });
            let body = format!(
                    "data: {{\"type\":\"response.output_text.delta\",\"item_id\":\"{message_id}\",\"output_index\":0,\"content_index\":0,\"delta\":\"verified\"}}\n\ndata: {}\n\n",
                    serde_json::json!({"type":"response.completed","response":completed})
                );
            write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
                .unwrap();
            stream.flush().unwrap();
        }
    });

    let profile = "buildmesh_pinned_contract";
    materialize_proxy_profile(
        EnvType::Windows,
        &install,
        profile,
        "Pinned fake Responses",
        &endpoint,
    )
    .unwrap();
    let run = |args: &[&str]| {
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("cmd.exe");
            command.args(["/d", "/c", &install.executable]);
            command
        } else {
            std::process::Command::new(&install.executable)
        };
        command
            .args(args)
            .current_dir(temp.path())
            .env("CODEX_HOME", temp.path())
            .env(PROXY_CREDENTIAL_ENV, "pinned-secret")
            .env_remove("OPENAI_API_KEY")
            .env_remove("OPENAI_BASE_URL")
            .output()
            .unwrap()
    };
    let fresh = run(&[
        "--profile",
        profile,
        "--model",
        "MiniMax-M3",
        "exec",
        "--skip-git-repo-check",
        "reply with verified",
    ]);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    let resume = run(&[
        "--profile",
        profile,
        "--model",
        "MiniMax-M3",
        "exec",
        "resume",
        "--last",
        "--skip-git-repo-check",
        "reply with verified again",
    ]);
    assert!(
        resume.status.success(),
        "{}",
        String::from_utf8_lossy(&resume.stderr)
    );
    server.join().unwrap();
}

#[test]
fn native_profile_repairs_stale_content_and_preserves_user_config() {
    let home = TempDir::new().unwrap();
    let user_config = home.path().join("config.toml");
    std::fs::write(&user_config, "model = \"user-choice\"\n").unwrap();
    let profile = "buildmesh_1234";
    let target = home.path().join(format!("{profile}.config.toml"));
    std::fs::write(&target, "edited = true\n").unwrap();
    let expected = render_proxy_profile(profile, "MiniMax", "https://api.minimax.io/v1");

    materialize_native_profile_at(home.path(), profile, &expected).unwrap();
    assert_eq!(std::fs::read_to_string(target).unwrap(), expected);
    assert_eq!(
        std::fs::read_to_string(user_config).unwrap(),
        "model = \"user-choice\"\n"
    );
}

#[test]
fn profile_materialization_fails_closed_when_home_is_not_a_directory() {
    let temp = TempDir::new().unwrap();
    let invalid_home = temp.path().join("codex-home-file");
    std::fs::write(&invalid_home, "occupied").unwrap();
    let error = materialize_native_profile_at(
        &invalid_home,
        "buildmesh_1234",
        "model_provider = \"buildmesh_1234\"\n",
    )
    .unwrap_err();
    assert!(error.contains("create Codex home"));
    assert_eq!(std::fs::read_to_string(invalid_home).unwrap(), "occupied");
}

#[test]
fn concurrent_profile_materialization_is_deterministic() {
    let home = TempDir::new().unwrap();
    let home_path = home.path().to_path_buf();
    let profile = "buildmesh_concurrent";
    let expected = render_proxy_profile(profile, "MiniMax", "https://api.minimax.io/v1");
    let mut workers = Vec::new();
    for _ in 0..8 {
        let home_path = home_path.clone();
        let expected = expected.clone();
        workers.push(std::thread::spawn(move || {
            materialize_native_profile_at(&home_path, profile, &expected)
        }));
    }
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(home.path().join(format!("{profile}.config.toml"))).unwrap(),
        expected
    );
}

#[test]
fn legacy_cleanup_requires_both_owned_name_and_exact_shape() {
    let home = TempDir::new().unwrap();
    let legacy_name = "bm1234567890abcdef";
    let owned = home.path().join(format!("{legacy_name}.config.toml"));
    let suspicious = home.path().join("bmabcdefabcdefabcd.config.toml");
    let user = home.path().join("bm-user.config.toml");
    std::fs::write(
            &owned,
            format!("model_provider = \"{legacy_name}\"\n\n[model_providers.{legacy_name}]\nname = \"Buildmesh proxy {legacy_name}\"\nbase_url = \"https://legacy.example/v1\"\nenv_key = \"OPENAI_API_KEY\"\nrequires_openai_auth = true\n"),
        )
        .unwrap();
    std::fs::write(
            &suspicious,
            "name = \"Buildmesh proxy bmabcdefabcdefabcd\"\nenv_key = \"OPENAI_API_KEY\"\nrequires_openai_auth = true\nuser_setting = true\n",
        )
        .unwrap();
    std::fs::write(&user, "name = \"my profile\"\n").unwrap();
    let expected = render_proxy_profile("buildmesh_new", "MiniMax", "https://example.com/v1");

    materialize_native_profile_at(home.path(), "buildmesh_new", &expected).unwrap();
    assert!(!owned.exists());
    assert!(suspicious.exists());
    assert!(user.exists());
}

/// Local hooks only run headlessly with the trust bypass (issue #884) —
/// both the fresh and resume spawn paths must carry the flag, or Codex
/// blocks on an interactive workspace-review prompt.
#[test]
fn spawn_recipes_carry_the_hook_trust_bypass() {
    let bypass = "--dangerously-bypass-hook-trust".to_string();
    let fresh = CODEX.spawn_recipe(Platform::Windows, EnvType::Windows);
    assert!(
        fresh.base_args.contains(&bypass),
        "fresh: {:?}",
        fresh.base_args
    );
    let resume = CODEX
        .spawn_recipe_for_resume(Platform::Windows, "sid-123")
        .expect("codex has a resume recipe");
    assert!(
        resume.base_args.contains(&bypass),
        "resume: {:?}",
        resume.base_args
    );
}

#[test]
// This guards Buildmesh's launch choice only; Codex's TUI owns the resize
// reflow behavior (`codex-rs/tui/src/app/resize_reflow.rs`). The separate
// `tests/unit/terminal-resize-scheduler.test.ts` covers only when Buildmesh
// forwards the PTY resize; neither test executes Codex's TUI.
fn spawn_recipes_allow_codex_fullscreen_transcript() {
    let no_alt_screen = "--no-alt-screen".to_string();
    let fresh = CODEX.spawn_recipe(Platform::Linux, EnvType::Wsl);
    assert!(
        !fresh.base_args.contains(&no_alt_screen),
        "fresh: {:?}",
        fresh.base_args
    );
    let resume = CODEX
        .spawn_recipe_for_resume(Platform::Linux, "sid-123")
        .expect("codex has a resume recipe");
    assert!(
        !resume.base_args.contains(&no_alt_screen),
        "resume: {:?}",
        resume.base_args
    );

    let fresh_windows = CODEX.spawn_recipe(Platform::Windows, EnvType::Windows);
    assert!(
        !fresh_windows.base_args.contains(&no_alt_screen),
        "fresh Windows: {:?}",
        fresh_windows.base_args
    );
    let resume_windows = CODEX
        .spawn_recipe_for_resume(Platform::Windows, "sid-123")
        .expect("codex has a Windows resume recipe");
    assert!(
        !resume_windows.base_args.contains(&no_alt_screen),
        "resume Windows: {:?}",
        resume_windows.base_args
    );
}

#[test]
fn effort_uses_codex_config_override() {
    assert_eq!(
        CODEX.effort_args("xhigh"),
        vec!["-c", "model_reasoning_effort=\"xhigh\""]
    );
}

#[test]
fn effort_config_override_escapes_embedded_quotes() {
    assert_eq!(
        CODEX.effort_args("weird\"name"),
        vec!["-c", r#"model_reasoning_effort="weird\"name""#]
    );
}

#[test]
fn codex_declares_attention_hook_and_readable_transcript() {
    assert!(CODEX.requires_attention_hook());
    assert!(CODEX.produces_readable_transcript());
    let crate::agent::capabilities::AttentionCapability::Hook {
        events,
        min_version,
        ..
    } = CODEX.attention_capability()
    else {
        panic!("Codex must expose native hook capability");
    };
    assert!(events.contains(&crate::agent::session_lifecycle::LifecycleKind::QuestionRequested));
    assert_eq!(min_version.as_deref(), Some(CODEX_MIN_HOOK_VERSION));
}

/// Every launch command must POST the hook's stdin JSON as the request
/// body: curl reads it from `@-` (escaped for PowerShell's parser where the
/// shell parses the text), and the native callback from the console.
fn forwards_hook_stdin(command: &str) -> bool {
    command.contains("--data-binary @-")
        || command.contains("--data-binary `@-")
        || (command.contains("[Console]::In.ReadToEnd()") && command.contains("Invoke-WebRequest"))
}

/// A Windows callback is installed as encoded PowerShell, so decode it
/// before asserting on the script it carries.
fn decoded_hook_command(command: &str) -> String {
    crate::env::decode_powershell_command(command).unwrap_or_else(|| command.to_string())
}

/// One `hooks.<Event>=…` launch argument, parsed by a real TOML parser.
struct LaunchHook {
    event: String,
    matcher: Option<String>,
    fields: std::collections::BTreeMap<String, String>,
}

fn launch_hooks(args: &[String]) -> Vec<LaunchHook> {
    assert_eq!(
        args.len() % 2,
        0,
        "every override is a `-c <key=value>` pair"
    );
    args.chunks(2)
        .filter_map(|pair| {
            assert_eq!(pair[0], "-c");
            let (key, value) = pair[1].split_once('=')?;
            let event = key.strip_prefix("hooks.")?;
            let document: DocumentMut = format!("{key} = {value}")
                .parse()
                .unwrap_or_else(|e| panic!("{key} is not valid TOML: {e}\n{value}"));
            let groups = document["hooks"][event].as_array().expect("event groups");
            assert_eq!(groups.len(), 1, "{event}");
            let group = groups.get(0).unwrap().as_inline_table().expect("group");
            let handlers = group.get("hooks").and_then(|h| h.as_array()).unwrap();
            assert_eq!(handlers.len(), 1, "{event}");
            let handler = handlers.get(0).unwrap().as_inline_table().unwrap();
            Some(LaunchHook {
                event: event.to_string(),
                matcher: group
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .map(str::to_string),
                fields: handler
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.as_str().unwrap().to_string()))
                    .collect(),
            })
        })
        .collect()
}

/// Launch arguments carry all seven attention events, each POSTing the
/// hook's stdin to *this node's* callback. The question-tool pre-hook
/// stays narrowly matched, while PostToolUse is catch-all so an approved
/// permission for any tool can clear its marker before the terminal Stop
/// fallback.
#[test]
fn launch_args_deliver_every_attention_event_to_this_node() {
    let args = attention_hook_launch_args(42, EnvType::Windows);
    assert_eq!(&args[..2], ["-c", "features.hooks=true"]);

    let hooks = launch_hooks(&args);
    let events: Vec<_> = hooks.iter().map(|hook| hook.event.as_str()).collect();
    assert_eq!(events, ATTENTION_HOOK_EVENTS);
    for hook in &hooks {
        let callback = decoded_hook_command(&hook.fields["commandWindows"]);
        assert!(
            callback.contains("/api/attention/42"),
            "{} must POST to node 42: {callback}",
            hook.event
        );
        assert!(
            forwards_hook_stdin(&callback),
            "{} must forward the hook stdin as the POST body: {callback}",
            hook.event
        );
        assert_eq!(hook.fields["type"], "command");
        assert_eq!(hook.fields["statusMessage"], BUILDMESH_HOOK_STATUS_MESSAGE);
        let expected_matcher = (hook.event == "PreToolUse").then_some(QUESTION_TOOL_MATCHER);
        assert_eq!(hook.matcher.as_deref(), expected_matcher, "{}", hook.event);
    }
}

/// Codex 0.162 asks with `request_user_input_async`; a matcher anchored
/// to the synchronous name never sent the ask, so the node read Running
/// while the user had a question waiting.
#[test]
fn question_matcher_selects_sync_and_async_question_tools() {
    let regex = regex::Regex::new(QUESTION_TOOL_MATCHER).unwrap();
    assert!(regex.is_match("request_user_input"));
    assert!(regex.is_match("request_user_input_async"));
    assert!(!regex.is_match("shell"));
    assert!(!regex.is_match("exec_command"));
}

/// Two nodes in one repository must never share a callback: each launch
/// names its own node and nothing is written to a shared file.
#[test]
fn concurrent_nodes_get_independent_callbacks() {
    let a = launch_hooks(&attention_hook_launch_args(41, EnvType::Windows));
    let b = launch_hooks(&attention_hook_launch_args(42, EnvType::Windows));
    for (a, b) in a.iter().zip(&b) {
        let a = decoded_hook_command(&a.fields["commandWindows"]);
        let b = decoded_hook_command(&b.fields["commandWindows"]);
        assert!(a.contains("/api/attention/41") && !a.contains("/attention/42"));
        assert!(b.contains("/api/attention/42") && !b.contains("/attention/41"));
    }
}

/// Windows PowerShell 5.1 drops double quotes passed to a native program,
/// so no launch argument may carry one. A single quote is legal in the TOML
/// syntax the argument is written in, but not inside the callback value it
/// wraps: `toml_launch_string` would then escape that value into a
/// double-quoted string. Newlines are out for the same reason. The whole
/// launch is re-encoded into one PowerShell script that has to fit inside
/// both of Windows' command-line budgets: 32,767 characters for the
/// process, and the 8,191 `cmd.exe` itself allows for the line it parses
/// when it runs an npm `codex.CMD` shim (#2184).
#[test]
fn windows_launch_args_are_quote_free_and_fit_the_command_line_budget() {
    for env_type in [EnvType::Windows, EnvType::WindowsInterop] {
        let args = attention_hook_launch_args(42, env_type);
        for arg in &args {
            assert!(!arg.contains('"'), "{env_type:?}: {args:?}");
            assert!(!arg.contains('\n'), "{env_type:?}: {args:?}");
            assert!(!arg.contains('\r'), "{env_type:?}: {args:?}");
        }
        for hook in launch_hooks(&args) {
            let callback = &hook.fields["commandWindows"];
            assert_eq!(hook.fields["command"], WINDOWS_COMMAND_STUB, "{env_type:?}");
            // The callback rides inside the TOML literal, so it must need
            // no escaping of its own.
            assert!(!callback.contains('\''), "{env_type:?}: {callback}");
            assert!(!callback.contains('"'), "{env_type:?}: {callback}");
        }
        // `' '` between arguments plus the two quotes around each.
        let script_chars: usize = args.iter().map(|arg| arg.len() + 3).sum();
        let encoded_chars = script_chars * 2 * 4 / 3;
        assert!(
            encoded_chars < 20_000,
            "{env_type:?}: hook arguments alone encode to {encoded_chars} of 32767 characters"
        );
        // What `cmd.exe` parses when PowerShell runs the shim: every argv
        // element quoted, plus the program and its switches.
        assert!(
            script_chars < crate::agent::spawn_environment::CMD_EXE_COMMAND_LINE_CHARS,
            "{env_type:?}: hook arguments alone consume {script_chars} of cmd.exe's {} \
                 characters, leaving no room for a prompt",
            crate::agent::spawn_environment::CMD_EXE_COMMAND_LINE_CHARS
        );
    }
}

/// A WSL runtime keeps a real `command` (its Linux Codex runs it, never
/// `commandWindows`) and, like Windows, stays free of quote characters.
#[test]
fn wsl_launch_args_carry_a_runnable_command_without_quotes() {
    let args = attention_hook_launch_args(42, EnvType::Wsl);
    assert!(args.iter().all(|arg| !arg.contains('"')), "{args:?}");
    for hook in launch_hooks(&args) {
        let command = &hook.fields["command"];
        assert!(command.contains("/api/attention/42"), "{command}");
        assert!(forwards_hook_stdin(command), "{command}");
        assert!(command.ends_with("printf {}"), "{command}");
    }
}

#[test]
fn long_single_line_prompts_go_through_the_pty_not_the_command_line() {
    let short = "x".repeat(MAX_POSITIONAL_PREFILL_CHARS);
    assert!(!CODEX.prefill_requires_pty(&short));
    let long = "x".repeat(MAX_POSITIONAL_PREFILL_CHARS + 1);
    assert!(CODEX.prefill_requires_pty(&long));
}

#[test]
fn toml_launch_string_prefers_literals_and_escapes_the_rest() {
    assert_eq!(toml_launch_string("plain $x"), "'plain $x'");
    // Backslashes and double quotes need no escaping inside a literal.
    assert_eq!(toml_launch_string(r#"a"b\c"#), r#"'a"b\c'"#);
    // A single quote or a newline forces the escaped basic form.
    assert_eq!(toml_launch_string("it's"), r#""it's""#);
    assert_eq!(toml_launch_string("it's \"a\\b\""), r#""it's \"a\\b\"""#);
    assert_eq!(toml_launch_string("a\nb"), r#""a\nb""#);
}

/// A bare `{}` prints nothing in PowerShell: the braces parse as a script
/// block. `echo ([char]123+[char]125)` spells the two bytes without a
/// quote character anywhere, which is the whole constraint on this value.
#[test]
fn attention_hook_commands_fail_open_and_emit_json() {
    let url = "http://localhost:1992/api/attention/42";

    let unix = attention_hook_unix_command(url);
    assert!(unix.contains("2>/dev/null || true; printf {}"), "{unix}");

    // The native arm, built directly so the assertion does not depend on
    // which host the test runs on; the WSL-host arm is `env`'s own test.
    let windows = crate::env::windows_hook_callback_command_for_runtime(url, None);
    assert!(!windows.contains('"'), "{windows}");
    assert!(!windows.contains('\''), "{windows}");
    assert!(!windows.contains('\n'), "{windows}");
    assert!(windows.contains("[Console]::In.ReadToEnd()"), "{windows}");
    assert!(windows.contains("Invoke-WebRequest"), "{windows}");
    assert!(windows.contains("catch { }"), "{windows}");
    assert!(
        windows.contains("echo ([char]123+[char]125)"),
        "the callback must print the empty JSON object without quoting it: {windows}"
    );
    assert!(windows.ends_with("exit 0"), "{windows}");
    assert!(windows.contains(url), "{windows}");
}

#[test]
fn codex_hook_relay_override_is_opt_in_and_loopback_only() {
    assert_eq!(
        attention_hook_url(42, 2992, Some("http://127.0.0.1:43123/")),
        "http://127.0.0.1:43123/api/attention/42?forward_port=2992"
    );
    for relay in [
        "https://127.0.0.1:43123",
        "http://example.test:43123",
        "http://127.0.0.1:0",
        "http://127.0.0.1:43123/path",
        "http://user@127.0.0.1:43123",
    ] {
        assert_eq!(
            attention_hook_url(42, 2992, Some(relay)),
            "http://localhost:2992/api/attention/42",
            "invalid relay must leave the app's local callback intact: {relay}"
        );
    }
    assert_eq!(
        attention_hook_url(42, 1992, None),
        "http://localhost:1992/api/attention/42"
    );
    assert!(base_flags_with_sandbox("read-only")
        .windows(2)
        .any(|pair| pair == ["--sandbox", "read-only"]));
    assert!(base_flags_with_sandbox("danger-full-access")
        .windows(2)
        .any(|pair| pair == ["--sandbox", "danger-full-access"]));
}

#[cfg(unix)]
#[test]
fn unix_attention_hook_emits_json_after_callback_failure() {
    use std::process::Command;

    let command = attention_hook_unix_command("http://localhost:1992/api/attention/42");
    let script = format!("set -e; curl() {{ return 22; }}; {command}");
    let output = Command::new("sh")
        .args(["-c", &script])
        .output()
        .expect("run attention hook in a POSIX shell");

    assert!(
        output.status.success(),
        "hook failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"{}");
}

/// The `command` a WSL node runs: a POSIX script, not the cmd-runnable
/// PowerShell a Windows node gets. `sh` has no `curl.exe`, so the probe
/// takes its Linux branch; stubbing `curl` then fails the callback, which
/// must still emit `{}`.
#[cfg(unix)]
#[test]
fn wsl_shell_attention_hook_emits_json_after_callback_failure() {
    use std::process::Command;

    let command = attention_hook_wsl_shell_command("http://localhost:1992/api/attention/42");
    let script = format!("set -e; curl() {{ return 22; }}; {command}");
    let output = Command::new("sh")
        .args(["-c", &script])
        .output()
        .expect("run the WSL attention hook in a POSIX shell");

    assert!(
        output.status.success(),
        "hook failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"{}");
}

#[cfg(windows)]
#[test]
fn windows_callback_emits_json_after_callback_failure_in_codex_runner() {
    let command = attention_hook_windows_command("http://localhost:1/api/attention/42");
    let output = run_like_codex(&command);

    assert!(
        output.status.success(),
        "hook failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{}");
}

/// Codex 0.162 evaluates a hook's `commandWindows` in PowerShell, not
/// `cmd.exe` (probed from a real session: `$PSVersionTable` expands,
/// `%CMDCMDLINE%` does not). A callback that only parses under cmd.exe is
/// a PowerShell syntax error there and shows as "hook exited with code 1".
#[cfg(windows)]
fn run_like_codex(command: &str) -> std::process::Output {
    use std::io::Write;

    let mut child = spawn_like_codex(command);
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(br#"{"hook_event_name":"Stop"}"#)
        .expect("write hook payload");
    child.wait_with_output().expect("wait for hook")
}

/// Codex's own runner shape: the hook command is handed to PowerShell as
/// one `-Command` argument (`codex_core::spawn`), with the hook's JSON on
/// stdin and its stdout captured.
#[cfg(windows)]
fn spawn_like_codex(command: &str) -> std::process::Child {
    use std::process::Stdio;

    let mut hook = crate::process_util::command_no_window("powershell.exe");
    // Codex's Core hook environment excludes the live app's callback address.
    hook.env_remove("BUILDMESH_PORT")
        .env_remove("BUILDMESH_SESSION_ID");
    hook.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        command,
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    hook.spawn().expect("spawn powershell.exe")
}

/// A loopback port nothing listens on, and one answering every request
/// `404` (a callback for a node Buildmesh no longer has).
#[cfg(windows)]
fn dead_and_not_found_urls() -> (String, String) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    server.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = server.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            };
            stream.set_nonblocking(false).ok();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    (
        format!("http://127.0.0.1:{dead_port}/api/attention/42"),
        format!("http://127.0.0.1:{port}/api/attention/42"),
    )
}

#[cfg(windows)]
#[test]
fn windows_attention_hook_emits_json_after_callback_failure_in_codex_runner() {
    let (dead, not_found) = dead_and_not_found_urls();
    for url in [dead, not_found] {
        // Rebuild both commands for the failure URL through the same
        // constructors `attention_hook_handler` uses for the live port.
        for (field, command) in [
            (
                "command",
                attention_hook_default_command(&url, EnvType::Windows),
            ),
            ("commandWindows", attention_hook_windows_command(&url)),
        ] {
            let output = run_like_codex(&command);
            assert!(
                output.status.success(),
                "{field} against {url} exited {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "{}",
                "{field} against {url}"
            );
        }
    }
}

/// Verbatim shapes earlier Buildmesh versions left in project
/// `.codex/hooks.json` files. They must stay red in the runner: if this
/// ever passes, `run_like_codex` has stopped being a faithful detector.
#[cfg(windows)]
#[test]
fn legacy_windows_callbacks_fail_in_codex_runner() {
    let (_dead, not_found) = dead_and_not_found_urls();
    let flags = "-fsS --connect-timeout 2 --max-time 10 -o NUL -X POST --data-binary @-";
    let cases = [
            (
                "env-var callback",
                "cmd.exe /c \"curl -sf -X POST --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID%\"".to_string(),
                None,
            ),
            (
                "unguarded curl",
                format!("curl.exe {flags} {not_found}"),
                None,
            ),
            (
                // cmd.exe-only `&` separator: a PowerShell parse error.
                "cmd.exe `& echo {}` suffix",
                format!("curl.exe {flags} {not_found} 2>NUL >NUL & echo {{}}"),
                Some(1),
            ),
        ];
    for (name, command, expected_code) in cases {
        let output = run_like_codex(&command);
        assert!(
            !output.status.success(),
            "{name} unexpectedly passed in the Codex runner"
        );
        if let Some(code) = expected_code {
            assert_eq!(output.status.code(), Some(code), "{name}");
        }
    }
}

/// Verbatim handler shapes earlier Buildmesh versions left in project
/// `.codex/hooks.json` files.
fn legacy_handlers() -> Vec<(String, serde_json::Value)> {
    let flags = "-fsS --connect-timeout 2 --max-time 10 -o NUL -X POST --data-binary @-";
    let url = "http://localhost:1992/api/attention/42";
    let posix = "if command -v curl.exe >/dev/null 2>&1; then curl.exe -fsS -o NUL -X POST --data-binary @- http://localhost:1992/api/attention/42 2>/dev/null || true; fi; printf '{}'";
    vec![
        (
            "env-var callback, no statusMessage".into(),
            serde_json::json!({
                "type": "command",
                "command": "cmd.exe /c \"curl -sf -X POST --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID%\"",
            }),
        ),
        (
            "unguarded curl".into(),
            serde_json::json!({
                "type": "command",
                "command": posix,
                "commandWindows": format!("curl.exe {flags} {url}"),
                "statusMessage": BUILDMESH_HOOK_STATUS_MESSAGE,
            }),
        ),
        (
            "cmd.exe `& echo {}` suffix".into(),
            serde_json::json!({
                "type": "command",
                "command": posix,
                "commandWindows": format!("curl.exe {flags} {url} 2>NUL >NUL & echo {{}}"),
                "statusMessage": BUILDMESH_HOOK_STATUS_MESSAGE,
            }),
        ),
        (
            "encoded PowerShell".into(),
            attention_hook_handler(42, EnvType::Windows),
        ),
    ]
}

/// Retiring drops every Buildmesh shape and nothing else: a user's own
/// handler (even one sharing a matcher group with a Buildmesh handler),
/// their other events and unrelated top-level keys all survive.
#[test]
fn retiring_removes_only_buildmesh_handlers_from_hooks_json() {
    let mine = serde_json::json!({ "type": "command", "command": "echo mine" });
    for (name, legacy) in legacy_handlers() {
        let existing = serde_json::json!({
            "extra": { "keep": true },
            "hooks": {
                "Stop": [{ "matcher": "m", "hooks": [legacy.clone(), mine.clone()] }],
                "SessionStart": [{ "hooks": [legacy.clone()] }],
                "PreCompact": [{ "hooks": [mine.clone()] }],
                "Untouched": [],
            },
        });

        let updated = strip_buildmesh_hook_handlers(&existing.to_string())
            .unwrap()
            .unwrap_or_else(|| panic!("{name}: a Buildmesh handler must be removed"));
        let updated: serde_json::Value = serde_json::from_str(&updated).unwrap();

        assert_eq!(
            updated,
            serde_json::json!({
                "extra": { "keep": true },
                "hooks": {
                    "Stop": [{ "matcher": "m", "hooks": [mine.clone()] }],
                    "PreCompact": [{ "hooks": [mine.clone()] }],
                    "Untouched": [],
                },
            }),
            "{name}"
        );
    }
}

#[test]
fn retiring_a_file_without_buildmesh_handlers_changes_nothing() {
    let mine = serde_json::json!({
        "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "echo mine" }] }] }
    });
    for existing in ["", "{}", r#"{"hooks":{}}"#, &mine.to_string()] {
        assert_eq!(strip_buildmesh_hook_handlers(existing).unwrap(), None);
    }
}

fn resolved_windows(dir: &Path) -> ResolvedPath {
    let path = dir.to_string_lossy().into_owned();
    ResolvedPath {
        host_path: path.clone(),
        spawn_path: path.clone(),
        raw_path: path,
        env_type: EnvType::Windows,
    }
}

fn retire(dir: &Path) {
    CODEX
        .provision_attention_hooks(&resolved_windows(dir), &LaunchRuntime::default(), 42)
        .unwrap();
}

fn write_hooks(dir: &Path, handlers: &[serde_json::Value]) -> std::path::PathBuf {
    let codex = dir.join(".codex");
    std::fs::create_dir_all(&codex).unwrap();
    let path = codex.join("hooks.json");
    let document = serde_json::json!({ "hooks": { "Stop": [{ "hooks": handlers }] } });
    std::fs::write(&path, document.to_string()).unwrap();
    path
}

fn stop_handlers(path: &Path) -> Vec<serde_json::Value> {
    let document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    document["hooks"]["Stop"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|group| group["hooks"].as_array().cloned().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

/// Provisioning is what clears a project that an older Buildmesh wrote:
/// the stale handlers go, the user's stay.
#[test]
fn provisioning_clears_stale_handlers_from_the_spawn_directory() {
    let temp = TempDir::new().unwrap();
    let mine = serde_json::json!({ "type": "command", "command": "echo mine" });
    let mut handlers: Vec<_> = legacy_handlers().into_iter().map(|(_, h)| h).collect();
    handlers.push(mine.clone());
    let path = write_hooks(temp.path(), &handlers);

    retire(temp.path());

    assert_eq!(stop_handlers(&path), vec![mine]);
}

/// Provisioning never creates Codex project files: hooks come from the
/// launch arguments, so a clean project stays clean.
#[test]
fn provisioning_creates_no_codex_project_files() {
    let temp = TempDir::new().unwrap();
    retire(temp.path());
    assert!(!temp.path().join(".codex").exists());
}

#[test]
fn provisioning_leaves_an_unparseable_hooks_file_for_codex_to_report() {
    let temp = TempDir::new().unwrap();
    let codex = temp.path().join(".codex");
    std::fs::create_dir_all(&codex).unwrap();
    let path = codex.join("hooks.json");
    std::fs::write(&path, "{not json").unwrap();

    retire(temp.path());

    assert_eq!(std::fs::read_to_string(path).unwrap(), "{not json");
}

/// From a linked worktree Codex loads the main checkout's `hooks.json`,
/// so that is the file whose stale Buildmesh handlers have to go.
#[test]
fn provisioning_clears_the_main_checkout_that_a_worktree_session_reads() {
    let temp = TempDir::new().unwrap();
    let main = temp.path().join("main");
    let worktree = main.join(".claude").join("worktrees").join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let git_dir = format!(
        "{}/.git/worktrees/wt",
        main.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(worktree.join(".git"), format!("gitdir: {git_dir}\n")).unwrap();
    let mine = serde_json::json!({ "type": "command", "command": "echo mine" });
    let stale = legacy_handlers().remove(2).1;
    let main_hooks = write_hooks(&main, &[stale.clone(), mine.clone()]);
    let worktree_hooks = write_hooks(&worktree, &[stale]);

    retire(&worktree);

    assert_eq!(stop_handlers(&main_hooks), vec![mine]);
    assert!(stop_handlers(&worktree_hooks).is_empty());
}

#[test]
fn linked_worktree_main_checkout_reads_the_git_file() {
    let temp = TempDir::new().unwrap();

    let plain = temp.path().join("plain");
    std::fs::create_dir_all(plain.join(".git")).unwrap();
    assert_eq!(linked_worktree_main_checkout(&plain, None), None);
    assert_eq!(
        linked_worktree_main_checkout(&temp.path().join("missing"), None),
        None
    );

    let linked = temp.path().join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    for (gitdir, main) in [
        (
            "gitdir: F:/src/pixelpath/.git/worktrees/wt",
            "F:/src/pixelpath",
        ),
        (
            r"gitdir: F:\src\pixelpath\.git\worktrees\wt",
            "F:/src/pixelpath",
        ),
        (
            "gitdir: /home/alice/repo/.git/worktrees/a-b",
            "/home/alice/repo",
        ),
    ] {
        std::fs::write(linked.join(".git"), format!("{gitdir}\n")).unwrap();
        assert_eq!(
            linked_worktree_main_checkout(&linked, None),
            Some(std::path::PathBuf::from(main)),
            "{gitdir}"
        );
    }
    // A submodule's gitdir is not a linked worktree.
    std::fs::write(linked.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
    assert_eq!(linked_worktree_main_checkout(&linked, None), None);
}

#[test]
fn project_trust_merge_adds_exact_path_and_preserves_other_projects() {
    let project = r#"F:\src\buildmesh\.claude\worktrees\linked"#;
    let existing = "model = \"gpt-5.2-codex\"\n\n[projects.\"F:\\\\src\\\\buildmesh\"]\ntrust_level = \"trusted\"\n\n[features]\nweb_search = true\n";
    let updated = ensure_project_trust_content(existing, project, EnvType::Windows).unwrap();

    assert!(updated.contains("model = \"gpt-5.2-codex\""));
    assert!(updated.contains("[projects.\"F:\\\\src\\\\buildmesh\"]"));
    assert!(updated.contains("[features]\nweb_search = true"));
    assert!(
        updated.contains("trust_level = \"trusted\""),
        "new project trust entry missing: {updated}"
    );
    assert_eq!(updated.matches("trust_level = \"trusted\"").count(), 2);
}

#[test]
fn project_trust_merge_updates_existing_untrusted_entry_in_place() {
    let project = r#"F:\src\buildmesh\.claude\worktrees\linked"#;
    let header = format!("[projects.{}]", toml_string(project));
    let existing = format!("{header}\ntrust_level = \"untrusted\"\nother = true\n");
    let updated = ensure_project_trust_content(&existing, project, EnvType::Windows).unwrap();

    assert_eq!(updated.matches(&header).count(), 1);
    assert_eq!(updated.matches("trust_level =").count(), 1);
    assert!(updated.contains("trust_level = \"trusted\""));
    assert!(updated.contains("other = true"));
}

#[test]
fn project_trust_merge_matches_comments_quote_styles_and_windows_case() {
    let project = r#"F:\src\buildmesh"#;
    let existing = r#"# [projects."F:\src\buildmesh"] is only a note
[projects.'f:\src\buildmesh'] # worktree root
trust_level = "untrusted" # preserve this explanation
"#;
    let updated = ensure_project_trust_content(existing, project, EnvType::Windows).unwrap();
    let document = updated.parse::<DocumentMut>().unwrap();
    let projects = document["projects"].as_table_like().unwrap();
    assert_eq!(
        projects.iter().count(),
        1,
        "must not create a duplicate table"
    );
    let (_, project_table) = projects.iter().next().unwrap();
    assert_eq!(
        project_table["trust_level"].as_str(),
        Some("trusted"),
        "the existing single-quoted project key must be updated"
    );
    assert!(updated.contains("# worktree root"));
    assert!(updated.contains("# preserve this explanation"));
}

#[test]
fn project_trust_merge_normalizes_windows_separators_and_trailing_slashes() {
    let existing = r#"[projects."F:/src/buildmesh/"]
trust_level = "untrusted"
"#;
    let updated =
        ensure_project_trust_content(existing, r#"F:\src\buildmesh"#, EnvType::Windows).unwrap();
    let document = updated.parse::<DocumentMut>().unwrap();
    let projects = document["projects"].as_table_like().unwrap();
    assert_eq!(projects.iter().count(), 1);
    assert_eq!(
        projects
            .get("F:/src/buildmesh/")
            .unwrap()
            .as_table_like()
            .unwrap()
            .get("trust_level")
            .and_then(Item::as_value)
            .and_then(|item| item.as_str()),
        Some("trusted")
    );
}

#[test]
fn project_trust_merge_keeps_case_distinct_wsl_paths() {
    let existing = r#"[projects."/home/alice/Code"]
trust_level = "trusted"
"#;
    let updated = ensure_project_trust_content(existing, "/home/alice/code", EnvType::Wsl).unwrap();
    let document = updated.parse::<DocumentMut>().unwrap();
    let projects = document["projects"].as_table_like().unwrap();
    assert_eq!(projects.iter().count(), 2);
    assert!(updated.contains("[projects.\"/home/alice/Code\"]"));
    assert!(updated.contains("[projects.\"/home/alice/code\"]"));
}

#[test]
fn wsl_write_script_reads_payloads_from_stdin() {
    assert!(WSL_ATOMIC_WRITE_FILES_SCRIPT.contains("IFS= read -r encoded"));
    assert!(!WSL_ATOMIC_WRITE_FILES_SCRIPT.contains("printenv"));
    assert!(!WSL_ATOMIC_WRITE_FILES_SCRIPT.contains("BUILDMESH_CODEX_CONTENT"));
}

#[test]
fn malformed_project_trust_toml_is_rejected_without_a_rewrite() {
    let existing = "[projects.\"broken\"\ntrust_level = \"untrusted\"\n";
    let error = ensure_project_trust_content(existing, "broken", EnvType::Windows).unwrap_err();
    assert!(error.contains("parse Codex trust config"));
}

#[test]
fn trust_project_path_uses_runtime_path_for_wsl() {
    let resolved = ResolvedPath {
        host_path: r#"\\wsl$\Ubuntu\home\alice\repo"#.to_string(), // allow-wsl-path
        spawn_path: "/home/alice/repo".to_string(),
        raw_path: "/home/alice/repo".to_string(),
        env_type: EnvType::Wsl,
    };
    assert_eq!(trust_project_path(&resolved), "/home/alice/repo");
}

/// The callback is PowerShell statement text, and Codex evaluates it in
/// PowerShell (`codex_core::spawn` invokes `powershell.exe -NoProfile
/// -Command`), so run it exactly that way: the hook JSON arrives on stdin
/// and the command must post those bytes unaltered and print `{}`. Both
/// hook fields carry the same text on a native Windows node.
#[cfg(windows)]
#[test]
fn windows_attention_hook_posts_utf8_stdin_through_powershell() {
    use std::io::Write;
    for field in ["command", "commandWindows"] {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/api/attention/42", server.server_addr());
        let handler = attention_hook_handler(42, EnvType::Windows);
        let original_url = format!(
            "http://localhost:{}/api/attention/42",
            crate::http_server::current_http_port()
        );
        let command = handler[field]
            .as_str()
            .unwrap_or_else(|| panic!("{field} hook missing"))
            .replace(&original_url, &endpoint);
        // The POST is the observable readiness signal; the budget only
        // has to outlast a cold shell start on a *loaded* machine —
        // the shell under test plus the hook's own PowerShell, per
        // iteration. The old 15 s bound sat close enough to the real
        // cost that concurrent `cargo test` runs elsewhere produced
        // `received == None` — the hook never arriving (issue #2173).
        // Measured under load: ~32 s for all four iterations.
        const HOOK_POST_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);
        let receiver = std::thread::spawn(move || {
            let mut request = server.recv_timeout(HOOK_POST_BUDGET).unwrap()?;
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let path = request.url().to_owned();
            request.respond(tiny_http::Response::empty(200)).unwrap();
            Some((path, body))
        });
        let mut child = spawn_like_codex(&command);
        let payload = r#"{"hook_event_name":"PostToolUse","session_id":"controlled-session","tool_name":"exec_command","tool_input":{"cmd":"echo café → 日本語"},"tool_response":"Done… 😀"}"#;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let received = receiver.join().unwrap();
        assert!(
            output.status.success(),
            "{field} command {command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            received,
            Some(("/api/attention/42".into(), payload.into())),
            "{field}"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{}");
    }
}

/// The field a runtime really executes: Windows runs `commandWindows`
/// (its `command` is a stub), WSL's Linux Codex runs `command`.
#[test]
fn launch_hooks_are_not_double_wrapped_and_do_not_rely_on_process_env() {
    for (env_type, field) in [
        (EnvType::Windows, "commandWindows"),
        (EnvType::Wsl, "command"),
    ] {
        for hook in launch_hooks(&attention_hook_launch_args(42, env_type)) {
            let event = hook.event.as_str();
            let value = decoded_hook_command(&hook.fields[field]);
            let value = &value;
            assert!(
                !value.contains("cmd.exe") && !value.contains("sh -c"),
                "{event} {field} must not nest a shell Codex already launches: {value}"
            );
            assert!(
                    !value.contains("BUILDMESH_PORT") && !value.contains("BUILDMESH_SESSION_ID"),
                    "{event} {field} must bake the callback URL; Codex hook env is a Core snapshot that drops BUILDMESH_*: {value}"
                );
            assert!(
                value.contains("/api/attention/42"),
                "{event} {field} must bake the node id into the callback URL: {value}"
            );
            assert!(
                value.contains("http://localhost:"),
                "{event} {field} must use localhost (WSL loopback relay), not 127.0.0.1: {value}"
            );
            assert!(
                forwards_hook_stdin(value),
                "{event} {field} must POST the hook stdin as the request body: {value}"
            );
            assert!(
                value.contains("-o /dev/null") || value.contains("Out-Null"),
                "{event} {field} must discard the HTTP response body: {value}"
            );
        }
    }
}

/// Issue #2036: `command` is the fallback a pre-0.131.0 Codex runs when it
/// does not know `commandWindows`, and it is also what a non-Windows Codex
/// always runs. It therefore has to be written for the shell that will
/// really parse it — the target Codex binary, not the Buildmesh host.
#[test]
fn command_is_written_for_the_target_codex_binary() {
    let url = "http://localhost:1992/api/attention/42";

    // A WSL node runs a Linux Codex on either host, so `command` reaches
    // `$SHELL -lc` and must stay a POSIX script.
    let wsl = attention_hook_default_command(url, EnvType::Wsl);
    assert!(
        crate::env::decode_powershell_command(&wsl).is_none(),
        "a WSL Codex parses `command` with $SHELL -lc, never PowerShell: {wsl}"
    );
    assert!(wsl.ends_with("printf {}"), "{wsl}");

    // Every runtime whose Codex is a Windows binary gets the cmd-runnable
    // callback in both fields.
    let windows_targets: &[EnvType] = if cfg!(windows) {
        &[EnvType::Windows, EnvType::WindowsInterop]
    } else {
        &[EnvType::WindowsInterop]
    };
    for env_type in windows_targets {
        assert_eq!(
            attention_hook_default_command(url, *env_type),
            attention_hook_windows_command(url),
            "{env_type} Codex falls back to `command` on pre-0.131.0 binaries, so it must \
                 be the same cmd-runnable callback as commandWindows"
        );
    }
}

/// `codex resume [OPTIONS] [SESSION_ID] [PROMPT]`. Flags after the UUID
/// become the optional prompt, so a restarted node "resumes" into a
/// garbage turn instead of restoring the conversation.
#[test]
fn resume_recipe_puts_options_before_session_id() {
    let resume = CODEX
        .spawn_recipe_for_resume(Platform::Windows, "sid-123")
        .expect("codex has a resume recipe");
    assert_eq!(resume.trailing_args, vec!["sid-123".to_string()]);
    assert!(
        !resume.base_args.iter().any(|arg| arg == "sid-123"),
        "session id must not live in base_args: {:?}",
        resume.base_args
    );
    let resume_at = resume
        .base_args
        .iter()
        .position(|arg| arg == "resume")
        .expect("resume subcommand");
    assert_eq!(
        resume_at, 0,
        "expected `codex resume [OPTIONS]` then trailing id, got {:?} + {:?}",
        resume.base_args, resume.trailing_args
    );
    // Issue #2151: the raw resume recipe is bare of approval flags —
    // `--ask-for-approval never` comes from the effective permission
    // mode in `default_prepare` (pinned by
    // `permission_mode_changes_argv_of_next_spawn` below), never from
    // a hidden argv.
    assert!(
        !resume
            .base_args
            .iter()
            .any(|arg| arg == "--ask-for-approval"),
        "raw resume recipe must carry no approval flags; got {:?}",
        resume.base_args
    );
}

/// Issue #2151 regression pin: the prepared resume argv carries the
/// unattended `--ask-for-approval never` (today's behavior) ahead of
/// the trailing session id, and prompt mode drops it.
#[test]
fn permission_mode_changes_argv_of_next_spawn() {
    use crate::agent::capabilities::{
        ResolvedAgentConfig, PERMISSION_MODE_PROMPT, PERMISSION_MODE_UNATTENDED,
    };
    use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};
    use crate::models::EnvType;

    fn prepared_argv(mode: Option<&str>) -> Vec<String> {
        let config = ResolvedAgentConfig {
            model: None,
            effort: None,
            extra_args: None,
            permission_mode: mode.map(str::to_string),
        };
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume {
                id: "sid-123",
                cause: crate::agent::spawn::ResumeCause::Explicit,
            },
            config: &config,
            prefill: None,
            sandbox: false,
        };
        default_prepare(&CODEX, input).recipe.base_args
    }

    // No stored value: today's unattended behavior is preserved, and
    // the approval flag stays ahead of the trailing session id.
    let default_argv = prepared_argv(None);
    let approval_at = default_argv
        .iter()
        .position(|arg| arg == "--ask-for-approval")
        .expect("default resume argv must carry --ask-for-approval");
    assert_eq!(
        default_argv.get(approval_at + 1).map(String::as_str),
        Some("never"),
        "resume argv must pair --ask-for-approval with never; got {default_argv:?}"
    );
    assert_eq!(
        default_argv.first().map(String::as_str),
        Some("resume"),
        "resume subcommand must stay first; got {default_argv:?}"
    );
    // Explicit unattended: same flag.
    assert!(
        prepared_argv(Some(PERMISSION_MODE_UNATTENDED))
            .windows(2)
            .any(|pair| pair == ["--ask-for-approval", "never"]),
        "unattended resume must carry --ask-for-approval never"
    );
    // Prompt: the approval pair is gone; sandbox + hook trust stay.
    let prompt_argv = prepared_argv(Some(PERMISSION_MODE_PROMPT));
    assert!(
        !prompt_argv.iter().any(|arg| arg == "--ask-for-approval"),
        "prompt resume must not carry --ask-for-approval; got {prompt_argv:?}"
    );
    assert!(
        prompt_argv.contains(&"--sandbox".to_string()),
        "prompt mode must not drop the sandbox control; got {prompt_argv:?}"
    );
    // The mode descriptor the setting UI renders stays in sync with
    // the argv the spawn path emits.
    let modes = CODEX.permission_modes();
    assert_eq!(modes.len(), 2);
    assert_eq!(modes[0].id, PERMISSION_MODE_UNATTENDED);
    assert!(modes[0].label.contains("--ask-for-approval"));
    assert_eq!(
        CODEX.default_permission_mode().as_deref(),
        Some(PERMISSION_MODE_UNATTENDED)
    );
}
