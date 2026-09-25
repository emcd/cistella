//! Wire dispatcher: request demultiplexing plus fatal-terminal semantics.
//!
//! Split from `client.rs` at the file-size limit; behavior moves
//! verbatim. The dispatcher owns the guest exchange, routes
//! responses by request id, and funnels every abnormal-exit path
//! through [`fatal_terminal`] so the death latch and the
//! shutdown-uncertain flag stay truthful: proven shutdown latches,
//! failed shutdown names uncertainty without latching.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{CistellaError, Result};
use crate::framework::guest::GuestHost;
use crate::framework::protocol::{Envelope, PROTOCOL_MAJOR};

/// Fails every pending entry with the exchange-death error for its
/// op. Shared quiet core: sets no latch by itself (the caller
/// decides proven vs uncertain first).
fn fail_pending(pending: &mut HashMap<String, Pending>, error: &str) {
    for (_, entry) in pending.drain() {
        let _ = entry.reply.send(Err(CistellaError::Protocol(format!(
            "guest terminated during {}: {error}",
            entry.op
        ))));
    }
}

/// Runs one fatal exchange path with its shutdown proof already
/// attempted (the caller evaluates `guest.shutdown()` first so the
/// ordering is visible at every call site): proven shutdown latches
/// dead-or-reaped and fails pending with the reason; failed
/// shutdown fails WITHOUT latching — the guest or a descendant may
/// live, so no residue check may run beside it — and names the
/// uncertainty while recording the proof failure for `close()` to
/// report. Returns None on proven death, Some(uncertainty detail)
/// otherwise. All four fatal arms route through here: one helper,
/// one fixup site, one pin.
///
/// Public for the deterministic fatal-terminal pin (a shutdown
/// `Result` injects directly instead of racing peer teardown
/// timing).
///
/// Record precedes the uncertain store: any thread observing
/// uncertain==true must also observe the report.
pub fn fatal_terminal(
    pending: &mut HashMap<String, Pending>,
    dead: &AtomicBool,
    uncertain: &AtomicBool,
    shutdown_report: &Mutex<Option<String>>,
    reason: &str,
    shutdown: Result<()>,
) -> Option<String> {
    match shutdown.map_err(|error| error.to_string()) {
        Ok(()) => {
            dead.store(true, Ordering::SeqCst);
            fail_pending(pending, reason);
            None
        }
        Err(detail) => {
            let composed = format!("{reason}; guest shutdown failed: {detail}");
            *shutdown_report.lock().expect("shutdown report lock") = Some(composed.clone());
            uncertain.store(true, Ordering::SeqCst);
            fail_pending(pending, &composed);
            Some(detail)
        }
    }
}

/// Bound for a single envelope send (capped even on uncapped
/// awaits: the wait is unbounded, the write never is).
const SEND_BOUND: Duration = Duration::from_secs(30);

/// Dispatcher poll cadence: stream reads never block past this
/// slice, so commands, deadlines, and detach marks stay live.
pub(crate) const DISPATCH_SLICE: Duration = Duration::from_millis(100);

/// One in-flight request tracked by the dispatcher.
///
/// Public only as a signature name for [`fatal_terminal`] (the
/// deterministic pin drives the helper without constructing
/// entries); fields stay crate-internal.
pub struct Pending {
    /// Caller reply channel.
    pub(crate) reply: mpsc::Sender<Result<Value>>,
    /// Expiry for bounded ops; `None` for uncapped await.
    pub(crate) deadline: Option<Instant>,
    /// Op name (deadline diagnostics only).
    pub(crate) op: String,
    /// True for await calls (pending frames kept, not forwarded).
    pub(crate) is_await: bool,
    /// Execution handle for await calls (detach addressing).
    pub(crate) exec: Option<String>,
}

/// Dispatcher commands from client threads.
pub(crate) enum Command {
    /// Send one op; terminal response (or expiry) goes to `reply`.
    Op {
        /// Operation name.
        op: String,
        /// Request payload.
        payload: Value,
        /// Response budget; `None` for uncapped await.
        timeout: Option<Duration>,
        /// Caller reply channel.
        reply: mpsc::Sender<Result<Value>>,
    },
    /// Shut the guest down and stop the dispatcher.
    Shutdown {
        /// Shutdown result channel.
        reply: mpsc::Sender<Result<()>>,
    },
    /// Retire one await entry by execution handle (caller
    /// cancelled and already returned detached): the late
    /// terminal drops instead of lingering to harness exit.
    /// Best-effort by exec handle, not strict-id correlation:
    /// concurrent awaits sharing one handle (replays, retries)
    /// retire together, which is safe (all callers already left).
    DetachByExec {
        /// Execution handle whose await entry retires.
        exec: String,
    },
}

/// Starts the dispatcher thread owning the guest exchange.
/// Every abnormal-exit path (send timeout, hard send failure,
/// stream death, protocol violation) routes through
/// [`fatal_terminal`]: proven shutdown latches dead-or-reaped
/// before pending callers fail; failed shutdown fails WITHOUT
/// latching and names the uncertainty instead. Local send
/// refusal (oversize) and orderly Shutdown never trip either
/// latch.
pub(crate) fn serve(
    mut guest: GuestHost<std::process::ChildStdout, std::process::ChildStdin>,
    frame_timeout: Duration,
    dead: Arc<AtomicBool>,
    shutdown_report: Arc<Mutex<Option<String>>>,
    uncertain: Arc<AtomicBool>,
) -> (mpsc::Sender<Command>, std::thread::JoinHandle<()>) {
    // Negotiated ceiling (post-hello): the dispatcher refuses
    // oversize at the same bound the guest was promised, not
    // the smaller default.
    let max_frame = guest.exchange_mut().max_frame();
    let (commands_tx, commands_rx) = mpsc::channel::<Command>();
    let worker = std::thread::spawn(move || {
        let mut pending: HashMap<String, Pending> = HashMap::new();
        // Explicitly retired ids (expiry, detach): late
        // terminals with these ids drop silently. Anything
        // else unknown is a protocol violation, not patience.
        let mut retired: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut next_id: u64 = 0;
        let mut stopping = false;
        let mut stream = crate::framework::stream::StreamReader::new(max_frame, frame_timeout);
        while !stopping {
            // Drain new commands without blocking the stream.
            while let Ok(command) = commands_rx.try_recv() {
                match command {
                    Command::Op {
                        op,
                        payload,
                        timeout,
                        reply,
                    } => {
                        next_id += 1;
                        let id = format!("c{next_id}");
                        let envelope = Envelope {
                            protocol: PROTOCOL_MAJOR,
                            id: id.clone(),
                            op: op.clone(),
                            payload: payload.clone(),
                        };
                        let deadline = Instant::now() + timeout.unwrap_or(Duration::ZERO);
                        // Uncapped await uses no deadline; the
                        // send itself stays bounded.
                        let send_by = if timeout.is_none() {
                            Instant::now() + SEND_BOUND
                        } else {
                            deadline
                        };
                        let frame_len = crate::framework::protocol::envelope_bytes(&envelope).len();
                        if frame_len > max_frame {
                            // Local refusal: our envelope exceeds
                            // the negotiated ceiling the guest was
                            // promised — never sent, guest
                            // untouched, latch clear. The caller
                            // sees the typed refusal; dispatch
                            // continues.
                            let _ = reply.send(Err(CistellaError::Contract(format!(
                                "frame length {frame_len} exceeds maximum {max_frame}"
                            ))));
                            continue;
                        }
                        match guest.exchange_mut().send(&envelope, send_by) {
                            Ok(()) => {}
                            Err(error) => {
                                // Uncertain exchange (write
                                // timeout or hard failure):
                                // partial bytes may already sit
                                // in the pipe and neither end
                                // resynchronizes, so the exchange
                                // is over either way. Quiesce
                                // FIRST (bounded shutdown/reap)
                                // so no residue check can run
                                // beside a live mutator; only
                                // then latch and fail, so the
                                // latch means dead-or-reaped and
                                // callers never observe it beside
                                // a running guest.
                                let reason = if crate::framework::protocol::is_write_timeout(&error)
                                {
                                    "guest send timed out"
                                } else {
                                    "guest send failed"
                                };
                                let uncertainty = fatal_terminal(
                                    &mut pending,
                                    &dead,
                                    &uncertain,
                                    &shutdown_report,
                                    reason,
                                    guest.shutdown(),
                                );
                                let message = match &uncertainty {
                                    None => format!("guest terminated during {op}: {reason}"),
                                    Some(detail) => format!(
                                        "guest terminated during {op}: {reason}; guest shutdown failed: {detail}"
                                    ),
                                };
                                let _ = reply.send(Err(CistellaError::Protocol(message)));
                                stopping = true;
                                break;
                            }
                        }
                        let is_await = op == crate::framework::protocol::AWAIT_RESULT_OP;
                        let exec = payload
                            .get("execution_handle")
                            .and_then(|handle| handle.as_str())
                            .map(str::to_string);
                        pending.insert(
                            id,
                            Pending {
                                reply,
                                deadline: timeout.map(|_| deadline),
                                op,
                                is_await,
                                exec,
                            },
                        );
                    }
                    Command::DetachByExec { exec } => {
                        let retired_ids: Vec<String> = pending
                            .iter()
                            .filter(|(_, entry)| entry.exec.as_deref() == Some(&exec))
                            .map(|(id, _)| id.clone())
                            .collect();
                        for id in retired_ids {
                            pending.remove(&id);
                            retired.insert(id);
                        }
                    }
                    Command::Shutdown { reply } => {
                        let _ = reply.send(guest.shutdown());
                        stopping = true;
                        break;
                    }
                }
            }
            if stopping {
                break;
            }
            // One stream slice through the persistent
            // assembler: trickled frames resolve across
            // slices instead of desynchronizing.
            match stream.poll_frame(guest.exchange_mut().reader_mut(), DISPATCH_SLICE) {
                Ok(Some(body)) => {
                    // Full envelope grammar (unknown fields,
                    // correlation, token shapes): the
                    // dispatcher never trusts a hand-parsed id
                    // match alone. The response op must equal
                    // the pending op; retired ids (expiry,
                    // detach) drop silently; anything else
                    // unsolicited fails the exchange rather
                    // than confusing later correlation.
                    let envelope = match crate::framework::protocol::parse_envelope(&body) {
                        Ok(envelope) => envelope,
                        Err(_) => {
                            let _ = fatal_terminal(
                                &mut pending,
                                &dead,
                                &uncertain,
                                &shutdown_report,
                                "malformed response frame",
                                guest.shutdown(),
                            );
                            break;
                        }
                    };
                    match pending.remove(&envelope.id) {
                        Some(entry) if entry.op == envelope.op => {
                            // `{pending: true}` heartbeat: kept,
                            // never forwarded; only the terminal
                            // redeems the caller.
                            let pending_frame = envelope
                                .payload
                                .get("pending")
                                .and_then(|pending| pending.as_bool())
                                .unwrap_or(false);
                            if entry.is_await && pending_frame {
                                // Await heartbeat: keep the
                                // entry, forward nothing. Only
                                // the terminal redeems the
                                // caller.
                                pending.insert(envelope.id, entry);
                            } else if !entry.is_await && pending_frame {
                                let _ = entry.reply.send(Err(CistellaError::Contract(
                                    "pending frame on non-await op".to_string(),
                                )));
                            } else {
                                let _ = entry.reply.send(Ok(envelope.payload));
                            }
                        }
                        Some(entry) => {
                            // Op mismatch: the response names a
                            // live request but answers a
                            // different operation — fail the
                            // caller, keep no ambiguity.
                            let _ = entry.reply.send(Err(CistellaError::Contract(
                                "response op mismatches request".to_string(),
                            )));
                        }
                        None if retired.contains(&envelope.id) => {
                            // Explicitly retired (expiry,
                            // detach): late terminal drops.
                        }
                        None => {
                            let _ = fatal_terminal(
                                &mut pending,
                                &dead,
                                &uncertain,
                                &shutdown_report,
                                "unsolicited response id",
                                guest.shutdown(),
                            );
                            break;
                        }
                    }
                }
                Ok(None) => {
                    // Idle slice: loop back for commands,
                    // deadlines, and detach marks.
                }
                Err(error) => {
                    if crate::framework::protocol::is_read_timeout(&error) {
                        // Idle slice (no bytes yet): loop back.
                    } else {
                        // EOF, truncation, oversize, IO, frame
                        // stall: the guest is gone or speaking
                        // garbage. Quiesce first (bounded
                        // shutdown/reap — a live-but-byzantine
                        // guest must die before any residue
                        // check runs), then fail everything
                        // pending with the real reason and stop.
                        // Residue duty belongs to the callers'
                        // on_guest_death paths, which this typed
                        // error triggers.
                        let _ = fatal_terminal(
                            &mut pending,
                            &dead,
                            &uncertain,
                            &shutdown_report,
                            &format!("guest stream failed: {error}"),
                            guest.shutdown(),
                        );
                        break;
                    }
                }
            }
            // Expire bounded ops; uncapped awaits never expire.
            let now = Instant::now();
            let expired: Vec<String> = pending
                .iter()
                .filter(|(_, entry)| entry.deadline.is_some_and(|deadline| now >= deadline))
                .map(|(id, _)| id.clone())
                .collect();
            for id in expired {
                if let Some(entry) = pending.remove(&id) {
                    retired.insert(id.clone());
                    let _ = entry.reply.send(Err(CistellaError::Protocol(format!(
                        "{} response timed out",
                        entry.op
                    ))));
                }
            }
        }
    });
    (commands_tx, worker)
}
