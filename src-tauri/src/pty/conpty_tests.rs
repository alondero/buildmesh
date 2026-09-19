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
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() { break; }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
            output.extend(bytes);
            if output.windows(b"FRAME_PROBE_DONE".len()).any(|s| s == b"FRAME_PROBE_DONE") { break; }
        }
    }
    child.kill().unwrap();
    drop(master);
    thread.join().unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("FRAME_PROBE_DONE"), "PTY did not deliver probe output: {output:?}");
    let frames: Vec<_> = output.split("\x1b[?2026h").skip(1).collect();
    assert_eq!(frames.len(), 12, "missing synchronized frames: {output:?}");
    for frame in frames {
        let end = frame.find("\x1b[?2026l").expect("frame end");
        assert!(frame[..end].contains("\x1b[6;9H"), "cursor restoration escaped the synchronized frame: {frame:?}");
        assert!(frame[..end].contains('*'), "animation escaped the synchronized frame: {frame:?}");
    }
}

#[test]
fn native_conpty_preserves_synchronized_cursor_frames() {
    let pair = crate::agent::spawn::open_pty_pair(24, 80).unwrap();
    let mut command = CommandBuilder::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script()]);
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    assert_frames(child, pair.master);
}

#[test]
fn owned_conpty_preserves_synchronized_cursor_frames() {
    let command = format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}", crate::env::encode_powershell(&script()));
    let (child, master) = crate::sandbox::conpty::spawn_in_appcontainer(&command, None, &[], 24, 80, None).unwrap();
    assert_frames(Box::new(child), Box::new(master));
}
