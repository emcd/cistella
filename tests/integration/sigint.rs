//! SIGINT-probe collection for the transport harness: the
//! READY-gated 0x03 delivery, disposition reap, and marker
//! observation behind `check_sigint_via_forkpty`, plus the
//! backend-free controls that execute every transition.
//! Split from `transport.rs` at the file-size limit.

use std::process::Command;

use super::helpers::*;

/// What the SIGINT probe observed, as facts. The probe
/// collects; `sigint_verdict` decides — the capture
/// collector's facts-vs-verdict split, applied to this
/// probe's causal chain (READY → delivered 0x03 → clean
/// exit → marker present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerState {
    /// `test -f` saw the trap's marker inside the deadline.
    Present,
    /// Deadline passed with the marker still absent.
    AbsentExpired,
    /// A podman exec failed: marker state unknown, which is
    /// failure evidence, never absence.
    ExecFailed,
    /// The marker was never looked for (an earlier premise
    /// already failed). Refuses like absence.
    Unchecked,
}

/// One SIGINT probe run, as facts. No I/O here.
#[derive(Debug, Clone, Copy)]
pub struct SigintFacts {
    pub ready: bool,
    pub delivered: bool,
    pub disposition: super::capture::ChildDisposition,
    pub marker: MarkerState,
}

impl SigintFacts {
    fn new() -> Self {
        Self {
            ready: false,
            delivered: false,
            disposition: super::capture::ChildDisposition::Unknown,
            marker: MarkerState::Unchecked,
        }
    }
}

/// The probe's causal rule, as a testable pure function:
/// READY observed, 0x03 delivered, natural Exited(0), marker
/// present, no exec failure. Every missing premise refuses.
pub fn sigint_verdict(facts: &SigintFacts) -> bool {
    facts.ready
        && facts.delivered
        && matches!(
            facts.disposition,
            super::capture::ChildDisposition::Exited(0)
        )
        && facts.marker == MarkerState::Present
}

/// The verdict rule, pinned without a backend: the full
/// causal chain verifies, and every missing premise refuses.
/// Backend-free (pure facts in, bool out), so these run in
/// the fast suite.
#[test]
fn sigint_verdict_admits_only_the_complete_chain() {
    use super::capture::ChildDisposition;
    let good = SigintFacts {
        ready: true,
        delivered: true,
        disposition: ChildDisposition::Exited(0),
        marker: MarkerState::Present,
    };
    assert!(sigint_verdict(&good), "complete chain verifies");
    for (name, facts) in [
        (
            "missing READY",
            SigintFacts {
                ready: false,
                ..good
            },
        ),
        (
            "undelivered 0x03",
            SigintFacts {
                delivered: false,
                ..good
            },
        ),
        (
            "nonzero exit",
            SigintFacts {
                disposition: ChildDisposition::Exited(1),
                ..good
            },
        ),
        (
            "signal death",
            SigintFacts {
                disposition: ChildDisposition::Signaled(libc::SIGTERM),
                ..good
            },
        ),
        (
            "unknown disposition",
            SigintFacts {
                disposition: ChildDisposition::Unknown,
                ..good
            },
        ),
        (
            "reaped elsewhere",
            SigintFacts {
                disposition: ChildDisposition::Unavailable,
                ..good
            },
        ),
        (
            "marker absent at expiry",
            SigintFacts {
                marker: MarkerState::AbsentExpired,
                ..good
            },
        ),
        (
            "exec failed",
            SigintFacts {
                marker: MarkerState::ExecFailed,
                ..good
            },
        ),
        (
            "marker unchecked",
            SigintFacts {
                marker: MarkerState::Unchecked,
                ..good
            },
        ),
    ] {
        assert!(!sigint_verdict(&facts), "{name} refuses");
    }
}

/// One nonblocking master read's outcome, in seam
/// vocabulary. The production impl reads the pty; scripted
/// stubs drive the deterministic controls.
#[derive(Debug, Clone)]
pub enum PtyRead {
    Data(Vec<u8>),
    WouldBlock,
    Eof,
    Error,
}

/// One marker-exec attempt's outcome. Only an exact
/// `present` verifies and only an exact `absent` may be
/// polled again — anything else (spawn failure, timeout,
/// nonzero status, unexpected output) is terminal unknown,
/// never retried into PASS.
#[derive(Debug, Clone)]
pub enum MarkerExec {
    Present,
    Absent,
    Failed(String),
}

/// Pure output mapping: the ONLY retryable success is an
/// exact `absent`. A later valid output must never mask an
/// earlier unknown, so anything else terminally fails.
pub fn classify_marker_output(stdout: &str) -> MarkerExec {
    match stdout.trim() {
        "present" => MarkerExec::Present,
        "absent" => MarkerExec::Absent,
        other => MarkerExec::Failed(format!("unexpected marker output: {other:?}")),
    }
}

/// Injectable collection operations for the SIGINT probe.
/// The probe collects through this seam; the production impl
/// drives the real pty and execs, scripted stubs drive the
/// deterministic controls below.
pub trait SigintOps {
    fn read(&mut self) -> PtyRead;
    fn deliver(&mut self) -> bool;
    fn poll_exit(&mut self) -> Option<super::capture::ChildDisposition>;
    fn exec_marker(&mut self, iter_end: std::time::Instant) -> MarkerExec;
}

/// Budgets for one collection run. Tests pass tiny values;
/// production passes the documented windows.
pub struct SigintBudgets {
    pub ready: std::time::Duration,
    pub reap: std::time::Duration,
    pub marker: std::time::Duration,
    pub poll_interval: std::time::Duration,
    pub marker_interval: std::time::Duration,
}

/// The probe's collection, executable without a backend:
/// READY loop, guarded delivery, disposition reap, marker
/// observation — each step gated on the previous one, so a
/// missing premise forbids every later operation. Returns
/// facts; `sigint_verdict` decides.
pub fn run_sigint_collection<O: SigintOps>(ops: &mut O, budgets: &SigintBudgets) -> SigintFacts {
    let mut facts = SigintFacts::new();
    // READY must be observed: expiry or a terminal read
    // without it fails the collection before anything is
    // delivered.
    let mut acc = Vec::new();
    let ready_end = std::time::Instant::now() + budgets.ready;
    while std::time::Instant::now() <= ready_end {
        match ops.read() {
            PtyRead::Data(chunk) => {
                acc.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&acc).contains("READY") {
                    facts.ready = true;
                    break;
                }
            }
            PtyRead::WouldBlock => std::thread::sleep(budgets.poll_interval),
            PtyRead::Eof | PtyRead::Error => break,
        }
    }
    if !facts.ready {
        eprintln!("sigint pty: READY never observed; 0x03 not delivered");
        return facts;
    }
    if !ops.deliver() {
        eprintln!("sigint pty: 0x03 delivery refused; trap never signaled");
        return facts;
    }
    eprintln!("SIGINT wrote 0x03");
    facts.delivered = true;
    // Reap capturing the actual disposition: only a natural
    // Exited(0) reaches the marker observation.
    let reap_end = std::time::Instant::now() + budgets.reap;
    while std::time::Instant::now() <= reap_end {
        if let Some(disposition) = ops.poll_exit() {
            facts.disposition = disposition;
            break;
        }
        std::thread::sleep(budgets.poll_interval);
    }
    if facts.disposition != super::capture::ChildDisposition::Exited(0) {
        eprintln!(
            "sigint pty disposition {:?} is not a clean exit; marker never observed",
            facts.disposition
        );
        return facts;
    }
    // Marker observation against a fixed outer deadline. The
    // remaining-reserve preflight means no exec starts that
    // cannot be reaped: exhaustion reads as expiry (no
    // marker seen in budget), distinct from an
    // attempted-command failure. Only an exact `absent` is
    // polled again; a `present` verifies.
    let marker_end = std::time::Instant::now() + budgets.marker;
    loop {
        let Some(iter_end) = super::helpers::poll_at(marker_end) else {
            eprintln!("sigint marker budget exhausted with no present observed");
            facts.marker = MarkerState::AbsentExpired;
            break;
        };
        match ops.exec_marker(iter_end) {
            MarkerExec::Present => {
                facts.marker = MarkerState::Present;
                break;
            }
            MarkerExec::Absent => std::thread::sleep(budgets.marker_interval),
            MarkerExec::Failed(why) => {
                eprintln!("sigint marker exec terminal: {why}");
                facts.marker = MarkerState::ExecFailed;
                break;
            }
        }
    }
    facts
}

/// Production operations: the real pty master, the forked
/// child, and podman execs bounded by the machinery.
pub struct PtySigintOps {
    pub file: std::fs::File,
    pub child: nix::unistd::Pid,
    pub cname: String,
}

impl SigintOps for PtySigintOps {
    fn read(&mut self) -> PtyRead {
        let mut buf = vec![0u8; 4096];
        match std::io::Read::read(&mut self.file, &mut buf) {
            Ok(n) if n > 0 => PtyRead::Data(buf[..n].to_vec()),
            Ok(_) => PtyRead::WouldBlock,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => PtyRead::WouldBlock,
            Err(_) => PtyRead::Error,
        }
    }

    fn deliver(&mut self) -> bool {
        use std::io::Write;
        self.file.write_all(&[0x03]).and(self.file.flush()).is_ok()
    }

    fn poll_exit(&mut self) -> Option<super::capture::ChildDisposition> {
        let mut reaped = false;
        let mut exited_at = None;
        let mut disposition = super::capture::ChildDisposition::Unknown;
        if super::capture::child_gone(self.child, &mut reaped, &mut exited_at, &mut disposition)
            && reaped
        {
            Some(disposition)
        } else {
            None
        }
    }

    fn exec_marker(&mut self, iter_end: std::time::Instant) -> MarkerExec {
        let mut cmd = Command::new("podman");
        cmd.args([
            "exec",
            &self.cname,
            "sh",
            "-c",
            "test -f /tmp/sigint_harness && echo present || echo absent",
        ]);
        match run_bounded_at(cmd, iter_end) {
            Err(why) => MarkerExec::Failed(format!("exec failed to run: {why:?}")),
            Ok(out) if !out.status.success() => MarkerExec::Failed(format!(
                "exec refused: {}",
                String::from_utf8_lossy(&out.stderr)
            )),
            Ok(out) => classify_marker_output(&String::from_utf8_lossy(&out.stdout)),
        }
    }
}

/// Scripted collection operations: every backend response
/// is queued up front, and every call is recorded, so the
/// controls below execute the actual collection mapping —
/// including what is NOT attempted after a terminal premise.
/// Queues pop from the end; push scripts in reverse.
struct ScriptOps {
    reads: Vec<PtyRead>,
    deliver: bool,
    exits: Vec<Option<super::capture::ChildDisposition>>,
    execs: Vec<MarkerExec>,
    calls: Vec<&'static str>,
}

impl ScriptOps {
    fn new() -> Self {
        Self {
            reads: Vec::new(),
            deliver: false,
            exits: Vec::new(),
            execs: Vec::new(),
            calls: Vec::new(),
        }
    }
}

impl SigintOps for ScriptOps {
    fn read(&mut self) -> PtyRead {
        self.calls.push("read");
        self.reads.pop().expect("script exhausted: unexpected read")
    }

    fn deliver(&mut self) -> bool {
        self.calls.push("deliver");
        self.deliver
    }

    fn poll_exit(&mut self) -> Option<super::capture::ChildDisposition> {
        self.calls.push("poll_exit");
        self.exits
            .pop()
            .expect("script exhausted: unexpected exit poll")
    }

    fn exec_marker(&mut self, _iter_end: std::time::Instant) -> MarkerExec {
        self.calls.push("exec");
        self.execs.pop().expect("script exhausted: unexpected exec")
    }
}

fn test_budgets() -> SigintBudgets {
    // Note: the marker budget must clear poll_at's
    // remaining-reserve preflight (REAP_ALLOWANCE +
    // POST_EXIT_DRAIN + 1s ≈ 21s) or no exec ever starts —
    // that preflight is itself under test below.
    SigintBudgets {
        ready: std::time::Duration::from_secs(5),
        reap: std::time::Duration::from_secs(5),
        marker: std::time::Duration::from_secs(25),
        poll_interval: std::time::Duration::from_millis(1),
        marker_interval: std::time::Duration::from_millis(1),
    }
}

#[test]
fn sigint_collection_missing_ready_forbids_delivery() {
    // EOF before READY: facts stay negative and delivery is
    // never attempted — no 0x03 to a shell that never
    // announced itself.
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Eof];
    let facts = run_sigint_collection(&mut ops, &test_budgets());
    assert!(!facts.ready);
    assert!(!facts.delivered);
    assert_eq!(ops.calls, vec!["read"]);
    assert!(!sigint_verdict(&facts));
}

#[test]
fn sigint_collection_write_failure_stops_before_reap() {
    // Delivery refused: no exit poll, no marker exec, and no
    // logged success — the trap was never signaled.
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = false;
    let facts = run_sigint_collection(&mut ops, &test_budgets());
    assert!(facts.ready);
    assert!(!facts.delivered);
    assert_eq!(ops.calls, vec!["read", "deliver"]);
    assert!(!sigint_verdict(&facts));
}

#[test]
fn sigint_collection_nonzero_exit_skips_marker() {
    // Exited(1): the marker observation never runs — a
    // nonzero exit proves the trap path, not the marker.
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = true;
    ops.exits = vec![Some(super::capture::ChildDisposition::Exited(1))];
    let facts = run_sigint_collection(&mut ops, &test_budgets());
    assert_eq!(
        facts.disposition,
        super::capture::ChildDisposition::Exited(1)
    );
    assert!(
        !ops.calls.contains(&"exec"),
        "no marker exec after bad exit"
    );
    assert!(!sigint_verdict(&facts));
}

#[test]
fn sigint_collection_signal_death_skips_marker() {
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = true;
    ops.exits = vec![Some(super::capture::ChildDisposition::Signaled(9))];
    let facts = run_sigint_collection(&mut ops, &test_budgets());
    assert!(
        !ops.calls.contains(&"exec"),
        "no marker exec after signal death"
    );
    assert!(!sigint_verdict(&facts));
}

#[test]
fn sigint_collection_absent_then_present_passes() {
    // The only retryable sequence: exact absent, then exact
    // present inside the budget.
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = true;
    ops.exits = vec![Some(super::capture::ChildDisposition::Exited(0))];
    ops.execs = vec![MarkerExec::Present, MarkerExec::Absent];
    let facts = run_sigint_collection(&mut ops, &test_budgets());
    assert_eq!(facts.marker, MarkerState::Present);
    assert_eq!(
        ops.calls.iter().filter(|c| **c == "exec").count(),
        2,
        "one absent poll, one present"
    );
    assert!(sigint_verdict(&facts));
}

#[test]
fn sigint_collection_final_absent_expires() {
    // Absent to the budget end: expiry, not failure — and
    // the verdict refuses. (Budget just over the preflight
    // reserve so real polls run before exhaustion.)
    let mut budgets = test_budgets();
    budgets.marker = std::time::Duration::from_secs(22);
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = true;
    ops.exits = vec![Some(super::capture::ChildDisposition::Exited(0))];
    ops.execs = vec![MarkerExec::Absent; 100_000];
    let facts = run_sigint_collection(&mut ops, &budgets);
    assert_eq!(facts.marker, MarkerState::AbsentExpired);
    assert!(!sigint_verdict(&facts));
}

#[test]
fn sigint_collection_unknown_stops_without_second_exec() {
    // THE masking case: an unknown first response is terminal.
    // A later present must not be observed, let alone pass.
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = true;
    ops.exits = vec![Some(super::capture::ChildDisposition::Exited(0))];
    ops.execs = vec![
        MarkerExec::Present,
        MarkerExec::Failed("unexpected marker output: \"garbled\"".to_string()),
    ];
    let facts = run_sigint_collection(&mut ops, &test_budgets());
    assert_eq!(facts.marker, MarkerState::ExecFailed);
    assert_eq!(
        ops.calls.iter().filter(|c| **c == "exec").count(),
        1,
        "unknown stops the loop; the queued present is never observed"
    );
    assert!(!sigint_verdict(&facts));
}

#[test]
fn sigint_collection_exec_refused_is_terminal() {
    // Nonzero exec status: terminal, single attempt.
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = true;
    ops.exits = vec![Some(super::capture::ChildDisposition::Exited(0))];
    ops.execs = vec![MarkerExec::Failed(
        "exec refused: exit status: 125".to_string(),
    )];
    let facts = run_sigint_collection(&mut ops, &test_budgets());
    assert_eq!(facts.marker, MarkerState::ExecFailed);
    assert_eq!(
        ops.calls.iter().filter(|c| **c == "exec").count(),
        1,
        "refused exec is terminal, never retried"
    );
    assert!(!sigint_verdict(&facts));
}

#[test]
fn sigint_collection_past_deadline_refuses_without_exec() {
    // Deadline already past: no exec starts, expiry recorded.
    let mut budgets = test_budgets();
    budgets.marker = std::time::Duration::ZERO;
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = true;
    ops.exits = vec![Some(super::capture::ChildDisposition::Exited(0))];
    ops.execs = vec![MarkerExec::Present];
    let facts = run_sigint_collection(&mut ops, &budgets);
    assert_eq!(facts.marker, MarkerState::AbsentExpired);
    assert!(
        !ops.calls.contains(&"exec"),
        "no exec starts past the deadline"
    );
    assert!(!sigint_verdict(&facts));
}

#[test]
fn sigint_collection_full_chain_verifies_end_to_end() {
    // Happy path through the actual collection: READY,
    // delivery, clean exit, present marker.
    let mut ops = ScriptOps::new();
    ops.reads = vec![PtyRead::Data(b"READY\n".to_vec())];
    ops.deliver = true;
    ops.exits = vec![Some(super::capture::ChildDisposition::Exited(0))];
    ops.execs = vec![MarkerExec::Present];
    let facts = run_sigint_collection(&mut ops, &test_budgets());
    assert!(sigint_verdict(&facts), "complete collected chain verifies");
}

#[test]
fn classify_marker_output_admits_only_exact_words() {
    assert!(matches!(
        classify_marker_output("present\n"),
        MarkerExec::Present
    ));
    assert!(matches!(
        classify_marker_output("absent"),
        MarkerExec::Absent
    ));
    for unknown in ["", "   ", "garbled", "Present", "PRESENT", "present absent"] {
        assert!(
            matches!(classify_marker_output(unknown), MarkerExec::Failed(_)),
            "{unknown:?} is terminal unknown, never retryable"
        );
    }
}
