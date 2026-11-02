//! Opt-in installed-harness smoke; this sends one small prompt to Codex.
use super::*;
use crate::agent::process::{AgentProcess, AgentProcessParams, InputQueueGauge};
use crate::agent::provider::adapters::CODEX;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

struct Cleanup(Arc<AgentProcessRegistry>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.kill_session(-930_121);
    }
}

#[test]
#[ignore = "requires BUILDMESH_PROMPT_SMOKE_EXE pointing to an authenticated Codex executable"]
fn installed_codex_receives_multiline_prompt_through_conpty() {
    let exe =
        std::env::var("BUILDMESH_PROMPT_SMOKE_EXE").expect("set installed Codex executable path");
    let pair = crate::agent::spawn::open_pty_pair(35, 79).unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut command = portable_pty::CommandBuilder::new(&exe);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.cwd(root.path());
    command.args([
        "--no-alt-screen",
        "--sandbox",
        "read-only",
        "--ask-for-approval",
        "never",
        "--dangerously-bypass-hook-trust",
    ]);
    command.args([
        "-c",
        "check_for_update_on_startup=false",
        "-c",
        "features.shell_tool=false",
        "-c",
        "features.multi_agent=false",
    ]);
    command.arg("-c");
    command.arg(format!(
        "projects.{}.trust_level=\"trusted\"",
        serde_json::to_string(&root.path().to_string_lossy()).unwrap()
    ));
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
    let queue = Arc::new(InputQueueGauge::default());
    let writer_queue = Arc::clone(&queue);
    let writer_handle = std::thread::spawn(move || {
        for bytes in rx {
            let result = writer.write_all(&bytes).and_then(|()| writer.flush());
            writer_queue.release(bytes.len());
            if result.is_err() {
                break;
            }
        }
    });
    let registry = Arc::new(AgentProcessRegistry::new());
    let _cleanup = Cleanup(Arc::clone(&registry));
    let alive = Arc::new(AtomicBool::new(true));
    registry.insert(
        -930_121,
        AgentProcess::new(AgentProcessParams {
            child,
            writer_tx: tx,
            queue,
            writer_handle: Some(writer_handle),
            master: pair.master,
            reader_alive: Arc::clone(&alive),
            deliberate_kill: Arc::new(AtomicBool::new(false)),
            job: None,
            reader_handle: None,
            spawn_start: Instant::now(),
            mesh_id: 0,
        }),
    );
    let process = registry.get(&-930_121).unwrap();
    crate::agent::process::watch_child_exit(
        Arc::clone(&process.child),
        Arc::clone(&process.master),
    );
    let output = Arc::clone(&process.delivery_output);
    let output_path =
        std::env::var_os("BUILDMESH_PROMPT_SMOKE_OUTPUT").map(std::path::PathBuf::from);
    let terminal = Arc::clone(&registry);
    process.set_reader_handle(std::thread::spawn(move || {
        // The real xterm answers these probes. Leaving a CLI's cursor query
        // pending can make its probe reader consume the first pasted bytes.
        let replies: &[(&[u8], &[u8])] = &[
            (b"\x1b[6n", b"\x1b[1;1R"),
            (b"\x1b[?6n", b"\x1b[?1;1R"),
            (b"\x1b[c", b"\x1b[?1;2c"),
            (b"\x1b[0c", b"\x1b[?1;2c"),
            (b"\x1b[>c", b"\x1b[>0;0;0c"),
            (b"\x1b[>0c", b"\x1b[>0;0;0c"),
        ];
        let mut probes = Vec::new();
        crate::agent::spawn::pump_pty_output(reader, |bytes| {
            probes.extend_from_slice(bytes);
            while let Some((at, query, reply)) = replies
                .iter()
                .filter_map(|(query, reply)| {
                    probes
                        .windows(query.len())
                        .position(|window| window == *query)
                        .map(|at| (at, *query, *reply))
                })
                .min_by_key(|(at, _, _)| *at)
            {
                assert!(terminal.write_input(-930_121, reply).unwrap().is_accepted());
                eprintln!("Answered terminal probe {query:?}");
                probes.drain(..at + query.len());
            }
            if probes.len() > 16 {
                probes.drain(..probes.len() - 16);
            }
            output.on_output(&String::from_utf8_lossy(bytes));
            if let Some(path) = &output_path {
                std::fs::write(path, output.snapshot(0, Instant::now()).text).unwrap();
            }
        });
        alive.store(false, Ordering::Release);
    }));
    let raw = format!("This is a terminal transport smoke check. Do not use tools or change files.\r\n{}\nReply with just PROMPT_SMOKE_OK.\rEnd of instructions.", "Keep this context within a single user message. ".repeat(10));
    let prompt = PreparedPrompt::for_adapter(-930_121, &CODEX, &raw);
    let stamp = registry.input_stamp(-930_121).unwrap();
    let result = deliver(
        &registry,
        &CODEX,
        DeliveryRequest {
            prompt: &prompt,
            mode: DeliveryMode::VerifiedStartup,
            observation: ObservationRequirement::Required,
            expected_input: Some(&stamp),
            still_current: &|| Ok(true),
        },
    );
    eprintln!("Codex ConPTY prompt delivery: {result:?}");
    assert_eq!(
        result.stop, None,
        "{result:?}; inspect the installed harness startup"
    );
    assert!(matches!(
        result.progress,
        DeliveryProgress::EnterAcknowledged { attempts: 1 }
    ));
    assert!(!prompt.text().contains('\n'));
    let expected = prompt.text().replace('\r', "\n");
    let deadline = Instant::now() + Duration::from_secs(15);
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("USERPROFILE").unwrap()).join(".codex")
        });
    let day = chrono::Utc::now().format("%Y/%m/%d").to_string();
    let mut accepted = false;
    while Instant::now() < deadline {
        for entry in std::fs::read_dir(home.join("sessions").join(&day)).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let contents = std::fs::read_to_string(&path).unwrap();
            // A unique temporary cwd binds this receipt to this smoke process.
            if contents
                .lines()
                .next()
                .and_then(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .is_none_or(|record| record["payload"]["cwd"].as_str() != root.path().to_str())
            {
                continue;
            }
            let matches = contents
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|record| {
                    record["type"] == "response_item"
                        && record["payload"]["type"] == "message"
                        && record["payload"]["role"] == "user"
                        && record["payload"]["content"]
                            .as_array()
                            .is_some_and(|content| {
                                content
                                    .iter()
                                    .any(|part| part["text"].as_str() == Some(expected.as_str()))
                            })
                })
                .count();
            assert!(
                matches <= 1,
                "the native transcript contains a duplicated user message"
            );
            accepted |= matches == 1;
        }
        if accepted {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        accepted,
        "native Codex transcript did not confirm the exact multiline user message"
    );
    eprintln!(
        "Native Codex transcript confirmed one user message with {} preserved line breaks",
        expected.matches('\n').count()
    );
}
