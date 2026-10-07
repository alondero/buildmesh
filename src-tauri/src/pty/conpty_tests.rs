//! Real Windows PTY contract: frame-complete must follow the final cursor move.
use portable_pty::{Child, CommandBuilder, MasterPty};
use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// The inbox ConPTY forwards unknown DEC 2026 markers immediately, but renders
// recognized cursor/text sequences later. This is the same ordering as Codex's
// animated composer; no model, account, or installed Codex is required.
const FRAME: &str = "\x1b[?2026h\x1b[?25l\x1b[3;20H*\x1b[0 q\x1b[6;9H\x1b[?25h\x1b[?2026l";

fn script() -> String {
    format!(
        "$frame = [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String('{}')); for ($i=0; $i -lt 12; $i++) {{ [Console]::Write($frame); Start-Sleep -Milliseconds 30 }}; [Console]::Write('FRAME_PROBE_DONE'); Start-Sleep -Seconds 10",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, FRAME)
    )
}

fn assert_frames(mut child: Box<dyn Child + Send + Sync>, master: Box<dyn MasterPty + Send>) {
    let mut reader = master.try_clone_reader().unwrap();
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut buf = [0; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    // Two-phase deadline, not one wall clock for everything (issue #2049).
    // Phase one waits for a cold `powershell.exe` (plus AppContainer setup
    // in the `spawn_in_appcontainer` variant) to start emitting: 10s was
    // under that on a loaded 24-core host and reported a false "PTY did
    // not deliver probe output". Probe content — the first synchronized
    // frame, or the done marker itself — ends that phase: the 12 frames
    // take ~360ms to emit, so from there a much tighter emission bound
    // applies, and a delivery regression fails in seconds instead of
    // stalling the suite for the whole start-up allowance. Earlier bytes
    // (shell banner, echo) prove nothing about frame delivery, so they
    // must not start the tight bound.
    // A child that exits before delivering the marker is a broken fixture
    // (or a dead child), never a slow start — but bytes the reader thread
    // forwarded around the exit still count, so the loop drains them
    // before judging. The child sleeps 10s after the marker on purpose,
    // which is why a healthy run stops on the probe marker rather than
    // on EOF.
    const SPAWN_BUDGET: Duration = Duration::from_secs(60);
    const EMIT_BUDGET: Duration = Duration::from_secs(10);
    // Grace to let bytes already in flight land after the child exits.
    const EXIT_DRAIN_GRACE: Duration = Duration::from_secs(2);
    const MARKER: &[u8] = b"FRAME_PROBE_DONE";
    const FRAME_START: &[u8] = b"\x1b[?2026h";
    let contains =
        |output: &[u8], needle: &[u8]| output.windows(needle.len()).any(|window| window == needle);
    let mut deadline = Instant::now() + SPAWN_BUDGET;
    let mut output = Vec::new();
    let mut child_exited_early = false;
    let mut emitting = false;
    while Instant::now() < deadline {
        if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
            output.extend(bytes);
            if !emitting && (contains(&output, FRAME_START) || contains(&output, MARKER)) {
                emitting = true;
                deadline = Instant::now() + EMIT_BUDGET;
            }
            if contains(&output, MARKER) {
                break;
            }
        }
        if child
            .try_wait()
            .map(|status| status.is_some())
            .unwrap_or(true)
        {
            let grace = Instant::now() + EXIT_DRAIN_GRACE;
            while !contains(&output, MARKER) && Instant::now() < grace {
                if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
                    output.extend(bytes);
                }
            }
            child_exited_early = !contains(&output, MARKER);
            break;
        }
    }
    let _ = child.kill();
    drop(master);
    thread.join().unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(
        !child_exited_early,
        "probe child exited before delivering FRAME_PROBE_DONE: {output:?}"
    );
    assert!(
        output.contains("FRAME_PROBE_DONE"),
        "PTY did not deliver probe output: {output:?}"
    );
    let frames: Vec<_> = output.split("\x1b[?2026h").skip(1).collect();
    assert_eq!(frames.len(), 12, "missing synchronized frames: {output:?}");
    for frame in frames {
        let end = frame.find("\x1b[?2026l").expect("frame end");
        assert!(
            frame[..end].contains("\x1b[6;9H"),
            "cursor restoration escaped the synchronized frame: {frame:?}"
        );
        assert!(
            frame[..end].contains('*'),
            "animation escaped the synchronized frame: {frame:?}"
        );
    }
}

#[test]
fn native_conpty_preserves_synchronized_cursor_frames() {
    let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
    let mut command = CommandBuilder::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &script(),
    ]);
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    assert_frames(child, pair.master);
}

#[test]
fn owned_conpty_preserves_synchronized_cursor_frames() {
    let command = format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        crate::env::encode_powershell(&script())
    );
    let (child, master) =
        crate::sandbox::conpty::spawn_in_appcontainer(&command, None, &[], 24, 80, None).unwrap();
    assert_frames(Box::new(child), Box::new(master));
}
