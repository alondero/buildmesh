use super::*;
use crate::agent::provider::adapters::{ANTHROPIC, CODEX, MUSE};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

enum Event {
    Output(String),
    HumanInput,
    Replacement,
    Exit,
    Receipt,
    Cancel,
}

struct ScriptedIo {
    start: Instant,
    elapsed: Cell<Duration>,
    alive: Cell<bool>,
    stamp: Cell<u64>,
    output: OutputObservation,
    events: RefCell<VecDeque<(Duration, Event)>>,
    refusals: RefCell<VecDeque<InputWriteError>>,
    writes: RefCell<Vec<Vec<u8>>>,
    receipt: Cell<Option<bool>>,
    acknowledge: Cell<bool>,
    current: Cell<bool>,
}

impl ScriptedIo {
    fn new(events: Vec<(u64, Event)>) -> Self {
        Self {
            start: Instant::now(),
            elapsed: Cell::new(Duration::ZERO),
            alive: Cell::new(true),
            stamp: Cell::new(0),
            output: OutputObservation::default(),
            events: RefCell::new(
                events
                    .into_iter()
                    .map(|(ms, event)| (Duration::from_millis(ms), event))
                    .collect(),
            ),
            refusals: RefCell::new(VecDeque::new()),
            writes: RefCell::new(Vec::new()),
            receipt: Cell::new(None),
            acknowledge: Cell::new(false),
            current: Cell::new(true),
        }
    }

    fn run(
        &self,
        adapter: &dyn AgentProvider,
        prompt: &PreparedPrompt,
        mode: DeliveryMode,
        observation: ObservationRequirement,
    ) -> DeliveryOutcome {
        execute(
            self,
            adapter,
            &DeliveryRequest {
                prompt,
                mode,
                observation,
                expected_input: Some("0"),
                still_current: &|| Ok(self.current.get()),
            },
            "0".into(),
        )
    }
}

impl DeliveryIo for ScriptedIo {
    fn now(&self) -> Instant {
        self.start + self.elapsed.get()
    }
    fn sleep(&self, duration: Duration) {
        self.elapsed.set(self.elapsed.get() + duration);
        loop {
            let next = self.events.borrow().front().map(|(at, _)| *at);
            if next.is_none_or(|at| at > self.elapsed.get()) {
                break;
            }
            let (_, event) = self.events.borrow_mut().pop_front().unwrap();
            match event {
                Event::Output(text) => self.output.record(&text, self.now()),
                Event::HumanInput | Event::Replacement => self.stamp.set(self.stamp.get() + 100),
                Event::Exit => self.alive.set(false),
                Event::Receipt => self.receipt.set(Some(true)),
                Event::Cancel => self.current.set(false),
            }
        }
    }
    fn alive(&self) -> bool {
        self.alive.get()
    }
    fn input_is_current(&self, stamp: &str) -> bool {
        stamp == self.stamp.get().to_string()
    }
    fn write(&self, bytes: &[u8], stamp: &str) -> Result<Option<String>, InputWriteError> {
        if !self.input_is_current(stamp) {
            return Ok(None);
        }
        if let Some(error) = self.refusals.borrow_mut().pop_front() {
            return Err(error);
        }
        self.writes.borrow_mut().push(bytes.to_vec());
        self.stamp.set(self.stamp.get() + 1);
        if bytes == b"\r" && self.acknowledge.get() {
            self.output.record("working", self.now());
        }
        Ok(Some(self.stamp.get().to_string()))
    }
    fn output(&self, since: u64) -> observation::Snapshot {
        self.output.snapshot(since, self.now())
    }
    fn receipt_accepted(&self) -> Option<bool> {
        self.receipt.get()
    }
}

#[test]
fn startup_handover_and_later_turn_transform_the_same_text_once() {
    let raw = "first\r\nsecond\nthird\rfourth";
    let prompt = PreparedPrompt::for_adapter(1, &CODEX, raw);
    assert_eq!(prompt.text(), "first\rsecond\rthird\rfourth");
    for mode in [
        DeliveryMode::OrdinaryStartup,
        DeliveryMode::VerifiedStartup,
        DeliveryMode::LaterTurn,
    ] {
        let io = ScriptedIo::new(vec![
            (250, Event::Output("› Ask Codex to do anything".into())),
            (2_000, Event::Output("[Pasted Content 25 chars]".into())),
        ]);
        io.acknowledge.set(true);
        let observation = if mode == DeliveryMode::OrdinaryStartup {
            ObservationRequirement::Unobserved
        } else {
            ObservationRequirement::Required
        };
        let result = io.run(&CODEX, &prompt, mode, observation);
        assert_eq!(result.stop, None, "{mode:?}: {result:?}");
        assert_eq!(
            *io.writes.borrow(),
            vec![
                b"\x1b[200~first\rsecond\rthird\rfourth\x1b[201~".to_vec(),
                b"\r".to_vec()
            ]
        );
        assert_eq!(
            result.progress,
            if observation == ObservationRequirement::Required {
                DeliveryProgress::EnterAcknowledged { attempts: 1 }
            } else {
                DeliveryProgress::EnterSent { attempts: 1 }
            }
        );
    }
}

#[test]
fn automatic_prefill_sends_no_enter_and_staged_prefill_requires_echo_and_quiet() {
    let prompt = PreparedPrompt::for_adapter(1, &CODEX, "Please review this complete task");
    let auto = ScriptedIo::new(vec![]);
    assert_eq!(
        auto.run(
            &CODEX,
            &prompt,
            DeliveryMode::AutoSubmitted,
            ObservationRequirement::Required
        )
        .progress,
        DeliveryProgress::AutoSubmitted
    );
    assert!(auto.writes.borrow().is_empty());
    let io = ScriptedIo::new(vec![
        (500, Event::Output("Do you trust this workspace?".into())),
        (5_000, Event::Output(prompt.text().into())),
    ]);
    io.acknowledge.set(true);
    assert_eq!(
        io.run(
            &CODEX,
            &prompt,
            DeliveryMode::Prefilled,
            ObservationRequirement::Required
        )
        .progress,
        DeliveryProgress::EnterAcknowledged { attempts: 1 }
    );
    assert_eq!(*io.writes.borrow(), vec![b"\r".to_vec()]);
    assert!(io.elapsed.get() >= Duration::from_millis(6_500));
}

#[test]
fn takeover_before_paste_between_paste_and_enter_and_during_retry_stops_writes() {
    let prompt = PreparedPrompt::for_adapter(1, &ANTHROPIC, "two\nlines");
    for (mode, events, expected_writes) in [
        (
            DeliveryMode::OrdinaryStartup,
            vec![(250, Event::HumanInput)],
            0,
        ),
        (DeliveryMode::LaterTurn, vec![(500, Event::HumanInput)], 1),
    ] {
        let io = ScriptedIo::new(events);
        let result = io.run(
            &ANTHROPIC,
            &prompt,
            mode,
            ObservationRequirement::Unobserved,
        );
        assert_eq!(result.stop, Some(DeliveryStop::Superseded));
        assert_eq!(io.writes.borrow().len(), expected_writes);
    }
    let io = ScriptedIo::new(vec![(4_000, Event::HumanInput)]);
    let result = io.run(
        &ANTHROPIC,
        &prompt,
        DeliveryMode::LaterTurn,
        ObservationRequirement::Required,
    );
    assert_eq!(result.stop, Some(DeliveryStop::Superseded));
    assert_eq!(
        *io.writes.borrow(),
        vec![
            injection_payload(prompt.text()).into_bytes(),
            b"\r".to_vec()
        ]
    );
}

#[test]
fn replacement_and_exit_during_readiness_are_distinct() {
    let prompt = PreparedPrompt::for_adapter(1, &CODEX, "do the work");
    for (event, expected) in [
        (Event::Replacement, DeliveryStop::Superseded),
        (Event::Exit, DeliveryStop::ProcessExited),
    ] {
        let io = ScriptedIo::new(vec![(500, event)]);
        let result = io.run(
            &CODEX,
            &prompt,
            DeliveryMode::VerifiedStartup,
            ObservationRequirement::Required,
        );
        assert_eq!(result.stop, Some(expected));
        assert!(io.writes.borrow().is_empty());
    }
}

#[test]
fn backpressure_retries_only_refused_bytes_and_checks_ownership_again() {
    let prompt = PreparedPrompt::for_adapter(1, &ANTHROPIC, "two\nlines");
    let io = ScriptedIo::new(vec![]);
    io.refusals
        .borrow_mut()
        .extend([InputWriteError::Backpressured; 2]);
    assert_eq!(
        io.run(
            &ANTHROPIC,
            &prompt,
            DeliveryMode::LaterTurn,
            ObservationRequirement::Unobserved
        )
        .stop,
        None
    );
    assert_eq!(io.writes.borrow().len(), 2);
    let io = ScriptedIo::new(vec![(40, Event::HumanInput)]);
    io.refusals
        .borrow_mut()
        .push_back(InputWriteError::Backpressured);
    assert_eq!(
        io.run(
            &ANTHROPIC,
            &prompt,
            DeliveryMode::LaterTurn,
            ObservationRequirement::Unobserved
        )
        .stop,
        Some(DeliveryStop::Superseded)
    );
    assert!(io.writes.borrow().is_empty());
    for refusal in [InputWriteError::Closed, InputWriteError::Backpressured] {
        let io = ScriptedIo::new(vec![]);
        io.refusals.borrow_mut().extend([refusal; 5]);
        assert_eq!(
            io.run(
                &ANTHROPIC,
                &prompt,
                DeliveryMode::LaterTurn,
                ObservationRequirement::Unobserved
            )
            .stop,
            Some(DeliveryStop::TransportFailure(refusal))
        );
        assert!(io.writes.borrow().is_empty());
    }
}

#[test]
fn ordinary_deadline_is_best_effort_and_circuit_deadline_remains_unverified() {
    let prompt = PreparedPrompt::for_adapter(1, &CODEX, "work");
    let ordinary = ScriptedIo::new(vec![]);
    let result = ordinary.run(
        &CODEX,
        &prompt,
        DeliveryMode::OrdinaryStartup,
        ObservationRequirement::Unobserved,
    );
    assert_eq!(result.progress, DeliveryProgress::EnterSent { attempts: 1 });
    assert_eq!(result.stop, None);
    assert_eq!(ordinary.elapsed.get(), Duration::from_secs(46));
    let circuit = ScriptedIo::new(vec![]);
    let result = circuit.run(
        &CODEX,
        &prompt,
        DeliveryMode::VerifiedStartup,
        ObservationRequirement::Required,
    );
    assert!(matches!(result.stop, Some(DeliveryStop::Unverified(_))));
    assert_eq!(circuit.elapsed.get(), Duration::from_secs(300));
    assert!(circuit.writes.borrow().is_empty());
}

#[test]
fn absent_acceptance_is_bounded_and_never_repastes_the_draft() {
    let prompt = PreparedPrompt::for_adapter(1, &ANTHROPIC, "work");
    let io = ScriptedIo::new(vec![]);
    let result = io.run(
        &ANTHROPIC,
        &prompt,
        DeliveryMode::LaterTurn,
        ObservationRequirement::Required,
    );
    assert_eq!(result.progress, DeliveryProgress::EnterSent { attempts: 3 });
    assert!(matches!(result.stop, Some(DeliveryStop::Unverified(_))));
    assert!(result.require_acknowledgement().is_err());
    assert_eq!(
        *io.writes.borrow(),
        vec![
            b"work".to_vec(),
            b"\r".to_vec(),
            b"\r".to_vec(),
            b"\r".to_vec()
        ]
    );
}

#[test]
fn codex_canonical_controls_transport_sanitation_and_composer_evidence_stay_bound() {
    let raw = "start\x1b[31m red\x1b[0m\x07\r\nnext\nlast\rend";
    let prompt = PreparedPrompt::for_adapter(1, &CODEX, raw);
    let canonical = prompt.text().to_owned();
    let io = ScriptedIo::new(vec![(
        250,
        Event::Output("[Pasted Content 23 chars]".into()),
    )]);
    io.acknowledge.set(true);
    let result = io.run(
        &CODEX,
        &prompt,
        DeliveryMode::LaterTurn,
        ObservationRequirement::Required,
    );
    assert_eq!(result.stop, None, "{result:?}");
    assert_eq!(prompt.text(), canonical);
    assert_eq!(canonical, "start\x1b[31m red\x1b[0m\x07\rnext\rlast\rend");
    assert_eq!(
        *io.writes.borrow(),
        vec![
            b"\x1b[200~start red\rnext\rlast\rend\x1b[201~".to_vec(),
            b"\r".to_vec()
        ]
    );
}

#[test]
fn midsize_codex_and_muse_pastes_wait_for_the_tail_through_the_operation() {
    let text = format!(
        "Review this change\n{} final instructions",
        "context ".repeat(70)
    );
    for adapter in [&CODEX as &dyn AgentProvider, &MUSE] {
        let prompt = PreparedPrompt::for_adapter(1, adapter, &text);
        let io = ScriptedIo::new(vec![
            (250, Event::Output("Review this change".into())),
            (10_000, Event::Output(text.clone())),
        ]);
        io.acknowledge.set(true);
        let result = io.run(
            adapter,
            &prompt,
            DeliveryMode::LaterTurn,
            ObservationRequirement::Required,
        );
        assert_eq!(result.stop, None);
        assert!(io.elapsed.get() >= Duration::from_secs(11));
        assert_eq!(io.writes.borrow().len(), 2);
    }
}

#[test]
fn receipt_requirement_ignores_redraw_and_accepts_only_the_receipt() {
    let prompt = PreparedPrompt::for_adapter(1, &MUSE, "review");
    let io = ScriptedIo::new(vec![
        (4_000, Event::Output("redraw".into())),
        (10_000, Event::Receipt),
    ]);
    io.receipt.set(Some(false));
    let result = io.run(
        &MUSE,
        &prompt,
        DeliveryMode::LaterTurn,
        ObservationRequirement::Required,
    );
    assert_eq!(
        result.progress,
        DeliveryProgress::AcceptanceObserved { attempts: 2 }
    );
    assert_eq!(result.stop, None);
}

#[test]
fn live_registry_generation_fences_a_request_even_with_an_independent_output_buffer() {
    let (registry, writes) = crate::agent::process::testing::capturing_registry(-930_120);
    let stamp = registry.input_stamp(-930_120).unwrap();
    let old = registry.get(&-930_120).unwrap();
    let output = Arc::clone(&old.delivery_output);
    registry.kill_session(-930_120);
    let prompt = PreparedPrompt::for_adapter(-930_120, &ANTHROPIC, "work");
    let result = deliver(
        &registry,
        &ANTHROPIC,
        DeliveryRequest {
            prompt: &prompt,
            mode: DeliveryMode::LaterTurn,
            observation: ObservationRequirement::Unobserved,
            expected_input: Some(&stamp),
            still_current: &|| Ok(true),
        },
    );
    assert_eq!(result.stop, Some(DeliveryStop::ProcessExited));
    assert!(writes.try_recv().is_err());
    crate::agent::process::testing::insert_standin(&registry, -930_120);
    let replacement = registry.get(&-930_120).unwrap();
    // The replacement wins after the outer facade selected its process Arc,
    // before a request without an explicit stamp captures input ownership.
    let raced = deliver_to_process(
        &registry,
        &old,
        &ANTHROPIC,
        DeliveryRequest {
            prompt: &prompt,
            mode: DeliveryMode::LaterTurn,
            observation: ObservationRequirement::Unobserved,
            expected_input: None,
            still_current: &|| Ok(true),
        },
    );
    assert_eq!(raced.stop, Some(DeliveryStop::Superseded));
    let result = deliver(
        &registry,
        &ANTHROPIC,
        DeliveryRequest {
            prompt: &prompt,
            mode: DeliveryMode::LaterTurn,
            observation: ObservationRequirement::Required,
            expected_input: Some(&stamp),
            still_current: &|| Ok(true),
        },
    );
    assert_eq!(result.stop, Some(DeliveryStop::Superseded));
    output.on_output("old reader output");
    assert_eq!(output.snapshot(0, Instant::now()).text, "old reader output");
    assert_eq!(
        replacement
            .delivery_output
            .snapshot(0, Instant::now())
            .cursor,
        0
    );
    registry.kill_session(-930_120);
}

#[test]
fn durable_cancellation_and_read_failure_never_authorize_a_write() {
    let prompt = PreparedPrompt::for_adapter(1, &ANTHROPIC, "work");
    for current in [Ok(false), Err("attempt lookup unavailable".to_string())] {
        let io = ScriptedIo::new(vec![]);
        let result = execute(
            &io,
            &ANTHROPIC,
            &DeliveryRequest {
                prompt: &prompt,
                mode: DeliveryMode::LaterTurn,
                observation: ObservationRequirement::Required,
                expected_input: Some("0"),
                still_current: &|| current.clone(),
            },
            "0".into(),
        );
        assert_eq!(
            result.stop,
            Some(match current {
                Ok(false) => DeliveryStop::Superseded,
                Err(reason) => DeliveryStop::Unverified(reason),
                _ => unreachable!(),
            })
        );
        assert!(io.writes.borrow().is_empty());
    }
}

#[test]
fn absent_rendered_paste_evidence_leaves_one_staged_prompt_without_enter() {
    let prompt = PreparedPrompt::for_adapter(1, &CODEX, "review\nthis change");
    let io = ScriptedIo::new(vec![(250, Event::Output("irrelevant redraw".into()))]);
    let result = io.run(
        &CODEX,
        &prompt,
        DeliveryMode::LaterTurn,
        ObservationRequirement::Required,
    );
    assert_eq!(result.progress, DeliveryProgress::TextStaged);
    assert!(matches!(result.stop, Some(DeliveryStop::Unverified(_))));
    assert_eq!(io.elapsed.get(), Duration::from_secs(90));
    assert_eq!(
        *io.writes.borrow(),
        vec![injection_payload(prompt.text()).into_bytes()]
    );
}

#[test]
fn output_observation_retains_a_bounded_utf8_tail_and_fresh_cursor() {
    let output = OutputObservation::default();
    let now = Instant::now();
    output.record(&"é".repeat(10_000), now);
    let snapshot = output.snapshot(0, now);
    assert!(snapshot.text.len() <= 16_000);
    assert_eq!(snapshot.cursor, 20_000);
    output.record("\x1b[31mready\x1b[0m", now + Duration::from_secs(1));
    let fresh = output.snapshot(snapshot.cursor, now + Duration::from_secs(2));
    assert_eq!(fresh.text, "ready");
    assert_eq!(fresh.quiet, Some(Duration::from_secs(1)));
}

#[test]
fn cancellation_during_prefill_readiness_and_submission_prevents_later_enter() {
    let prompt = PreparedPrompt::for_adapter(1, &CODEX, "Please review this complete task");
    for (cancel_at, expected_writes) in [(500, 0), (2_500, 1)] {
        let io = ScriptedIo::new(vec![
            (250, Event::Output(prompt.text().into())),
            (cancel_at, Event::Cancel),
        ]);
        let result = io.run(
            &CODEX,
            &prompt,
            DeliveryMode::Prefilled,
            ObservationRequirement::Required,
        );
        assert_eq!(result.stop, Some(DeliveryStop::Superseded));
        assert_eq!(io.writes.borrow().len(), expected_writes);
    }
}

#[test]
fn redraw_acknowledgement_does_not_claim_native_acceptance() {
    let prompt = PreparedPrompt::for_adapter(1, &ANTHROPIC, "work");
    let io = ScriptedIo::new(vec![(3_250, Event::Output("dialog redraw".into()))]);
    let result = io.run(
        &ANTHROPIC,
        &prompt,
        DeliveryMode::LaterTurn,
        ObservationRequirement::Required,
    );
    assert_eq!(
        result.progress,
        DeliveryProgress::EnterAcknowledged { attempts: 1 }
    );
    assert_eq!(result.stop, None);
}
