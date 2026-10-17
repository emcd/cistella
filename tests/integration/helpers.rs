//! Shared helpers for lifecycle integration tests.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub use super::bounded::*;
pub use super::gates::*;
pub use super::pty::*;

use cistella::isolators::client::WireClient;

/// Close-on-unwind owner for `WireClient` (which has no Drop):
/// holds the client from hosting until the test takes it for its
/// explicit consuming close. Any panic in between closes
/// at-most-once through Drop — `close(self)` consumes, so
/// `Option::take` guarantees a single close, never double (not
/// idempotence in the API sense, at-most-once in the ownership
/// sense).
pub struct CloseGuard(pub Option<WireClient>);

impl CloseGuard {
    #[must_use]
    pub fn new(client: WireClient) -> Self {
        Self(Some(client))
    }

    /// Borrows the held client for `&self` trait calls.
    #[must_use]
    pub fn get(&self) -> &WireClient {
        self.0.as_ref().expect("client held")
    }

    /// Reclaims the client for its explicit consuming close.
    #[must_use]
    pub fn take(&mut self) -> WireClient {
        self.0.take().expect("client held")
    }
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        if let Some(client) = self.0.take() {
            let _ = client.close();
        }
    }
}

/// Arc twin for thread-shared clients: Drop reclaims ONLY when
/// uniquely held (all clones dropped, e.g. after joining). A still
/// shared Arc (wedged thread) reports honestly instead of
/// pretending to clean up — no safe consuming close exists there.
pub struct ArcCloseGuard(pub Option<std::sync::Arc<WireClient>>);

impl ArcCloseGuard {
    #[must_use]
    pub fn new(client: std::sync::Arc<WireClient>) -> Self {
        Self(Some(client))
    }

    #[must_use]
    pub fn get(&self) -> &std::sync::Arc<WireClient> {
        self.0.as_ref().expect("client held")
    }

    #[must_use]
    pub fn take(&mut self) -> std::sync::Arc<WireClient> {
        self.0.take().expect("client held")
    }
}

impl Drop for ArcCloseGuard {
    fn drop(&mut self) {
        if let Some(arc) = self.0.take() {
            match std::sync::Arc::try_unwrap(arc) {
                Ok(client) => {
                    let _ = client.close();
                }
                Err(arc) => {
                    eprintln!(
                        "ArcCloseGuard: wedged holder; worker/rendezvous residue possible (refs={})",
                        std::sync::Arc::strong_count(&arc),
                    );
                }
            }
        }
    }
}

pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cistella")
}

pub fn home_dir() -> String {
    std::env::var("HOME").expect("HOME set")
}

pub fn run_cistella(home: &str, args: &[&str]) -> Output {
    run_bounded(spawn_cistella_cmd(home, args), RUN_CISTELLA_TIMEOUT).expect("bounded cistella run")
}

/// Owned-session teardown WITHOUT panicking: the `Drop` path.
/// Returns the terminate outcome for the caller to log; a `Drop`
/// that panics (e.g. on a cleanup timeout) during an assertion
/// unwind would double-panic and abort the test process, masking
/// the original failure and bypassing sibling cleanup.
pub fn guard_terminate(home: &str, id: &str) -> Result<Output, BoundedRunError> {
    run_bounded(
        spawn_cistella_cmd(home, &["terminate", id]),
        RUN_CISTELLA_TIMEOUT,
    )
}

pub fn valid_minted_id(id: &str) -> bool {
    id.len() == cistella::session::MINTED_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

pub fn unit_path(home: &str, id: &str) -> PathBuf {
    PathBuf::from(home)
        .join(".config/containers/systemd")
        .join(format!("cistella-{id}.container"))
}

/// Isolates one child conduct's temp root: production
/// conduct builds `cistella-rdv-{id}` under
/// `std::env::temp_dir()` ahead of the creation lock, so a
/// test asserting pre-mutation refusal passes `TMPDIR`
/// pointing here and asserts emptiness after — per-child
/// attribution for pre-lock residue that a shared-root
/// snapshot cannot give.
pub fn child_tmp_root() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("child temp root")
}

/// Asserts the isolated child temp root holds no entries:
/// any leftover is stranded guest-hosting residue from a
/// refusal sunk below hosting. Names leftovers on failure.
pub fn assert_child_tmp_empty(root: &tempfile::TempDir) {
    let leftovers: Vec<_> = std::fs::read_dir(root.path())
        .expect("child temp root readable")
        .filter_map(|entry| {
            entry
                .ok()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "refused conduct stranded temp entries: {leftovers:?}"
    );
}

/// Shared-root tmp names that are never the isolated
/// child's products: with its temp root redirected above,
/// any `cistella-rdv-*` left in the shared root is a
/// concurrent seat actor's wire-test rendezvous. Callers
/// scope these out of both snapshots; the creation lock
/// excludes concurrent conducts, so any other addition is
/// the probe's own residue.
pub fn is_foreign_tmp(name: &str) -> bool {
    name.starts_with("cistella-rdv-")
}

/// Guard removes the session even when an assertion fails.
pub struct Guard {
    pub id: Option<String>,
}

impl Guard {
    pub fn empty() -> Self {
        Self { id: None }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Owned teardown ONLY: terminate our own session id. Never a
        // global `gc` — reaping is a coordinated act (creation
        // lock), and a Guard drop running suite-wide gc reaps
        // sibling fixtures sitting in their unlocked windows. A
        // panic path that strands an orphaned unit surfaces it in
        // residue-diff tests instead of sweeping it silently.
        // Non-panicking by construction (`guard_terminate` returns
        // `Result`): a Drop panic during an assertion unwind would
        // abort the process and mask the original failure.
        if let Some(id) = self.id.take() {
            let home = home_dir();
            if let Err(why) = guard_terminate(&home, &id) {
                eprintln!("guard teardown of {id} failed (best effort): {why}");
            }
        }
    }
}

/// Filesystem fixture profile path (`tests/data/profiles/`): live
/// tests must never resolve baked names, so conducts take explicit
/// fixture paths (compile-time absolute, independent of test cwd).
#[must_use]
pub fn fixture_profile(name: &str) -> String {
    format!("{}/tests/data/profiles/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Spawns `conduct -- sleep 300` in the background and returns the child
/// plus the minted id from its first stdout line.
pub fn spawn_conduct(home: &str, worktree: &str, extra: &[&str]) -> (Child, String, Guard) {
    let profile = fixture_profile("default.toml");
    spawn_conduct_full(
        home,
        &profile,
        worktree,
        extra,
        &["sleep", "300"],
        &[],
        None,
    )
}

/// Spawns `conduct` with explicit profile, harness argv,
/// environment, and optional working directory (profile resolution
/// must not depend on cwd; the foreign-cwd test pins that with
/// `Some`).
pub fn spawn_conduct_full(
    home: &str,
    profile: &str,
    worktree: &str,
    extra: &[&str],
    argv: &[&str],
    envs: &[(&str, &str)],
    cwd: Option<&std::path::Path>,
) -> (Child, String, Guard) {
    let (child, id, guard, _) =
        spawn_conduct_tapped(home, profile, worktree, extra, argv, envs, cwd);
    (child, id, guard)
}

/// Bounded tail-retained stderr tap for one conduct child.
///
/// The drain thread feeds this instead of the sink, so a boundary test
/// can read conduct's OWN stderr after exit (an `inspect` post-mortem
/// is backend/unit/journal evidence, never conduct stderr). The cap
/// keeps memory bounded and retains the tail — the most
/// diagnostic-relevant bytes; the pipe stays open either way, so
/// SIGPIPE hygiene is unchanged.
///
/// Reads before completion see a prefix of the tail, never garbage;
/// use `finalize` after the child exits for the complete,
/// completion-attested evidence.
pub struct ConductTaps {
    stderr_tail: std::sync::Arc<std::sync::Mutex<TailBuffer>>,
    drain_done: std::sync::mpsc::Receiver<DrainOutcome>,
    drain_handle: Option<std::thread::JoinHandle<()>>,
}

/// How the stderr drain thread finished. `pub` only because
/// `run_tap_drain` (shared with deterministic pins) names it;
/// constructed by the worker alone.
#[derive(Debug)]
pub struct DrainOutcome {
    /// First read error text, if any (None = clean EOF).
    pub error: Option<String>,
}

/// Completion-attested retained evidence.
pub struct FinalizedTap {
    /// Retained stderr tail (complete iff `drain_clean`).
    pub stderr: String,
    /// True only when the drain thread observed EOF with no read error.
    pub drain_clean: bool,
    /// Human-readable drain disposition (clean, error text, or timeout).
    pub drain_note: String,
}

/// Bytes of retained conduct stderr to keep (tail).
const CONDUCT_TAP_CAP: usize = 32 * 1024;

/// Bounded tail buffer shared by the drain worker and tap readers.
/// `pub` only because `run_tap_drain` (shared with deterministic pins)
/// names it; construction and writes stay module-private.
pub struct TailBuffer {
    buf: Vec<u8>,
}

impl TailBuffer {
    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        let excess = self.buf.len().saturating_sub(CONDUCT_TAP_CAP);
        self.buf.drain(..excess);
    }
}

impl ConductTaps {
    /// Reads the retained stderr tail (lossy). Call after the child
    /// exits and the drain thread has observed EOF; concurrent reads
    /// mid-flight see a prefix of the tail, never garbage.
    /// Prefer `finalize` for completion-attested evidence.
    pub fn stderr_tail(&self) -> String {
        let guard = self.stderr_tail.lock().expect("tap mutex");
        String::from_utf8_lossy(&guard.buf).to_string()
    }

    /// Waits (bounded) for the drain thread to observe EOF, reaps it,
    /// and returns the completion-attested tail.
    ///
    /// `child.wait()` alone does not guarantee the drain consumed the
    /// final bytes — grandchildren can hold the pipe, and scheduling
    /// can lag. A timeout (or drain error) surfaces honestly in the
    /// returned note instead of passing on partial evidence silently.
    /// The reaping join runs ONLY after the drain reported completion:
    /// joining unconditionally after a timeout would block until EOF,
    /// making the timeout branch unreachable precisely when it
    /// matters (descendant-held pipe). On timeout the handle stays
    /// parked (detached with the test) and the partial tail is
    /// labeled unproven.
    pub fn finalize(&mut self, timeout: Duration) -> FinalizedTap {
        let outcome = self.drain_done.recv_timeout(timeout);
        let stderr = self.stderr_tail();
        match outcome {
            Ok(DrainOutcome { error: None }) => {
                if let Some(handle) = self.drain_handle.take() {
                    let _ = handle.join();
                }
                FinalizedTap {
                    stderr,
                    drain_clean: true,
                    drain_note: "drain clean EOF".to_string(),
                }
            }
            Ok(DrainOutcome { error: Some(error) }) => {
                if let Some(handle) = self.drain_handle.take() {
                    let _ = handle.join();
                }
                FinalizedTap {
                    stderr,
                    drain_clean: false,
                    drain_note: format!("drain read error: {error}"),
                }
            }
            Err(_) => FinalizedTap {
                stderr,
                drain_clean: false,
                drain_note: format!(
                    "drain not complete within {}ms (partial tail, unproven)",
                    timeout.as_millis()
                ),
            },
        }
    }
}

/// Spawns the stderr drain worker over any reader: feeds the bounded
/// tail buffer until EOF/error, then reports completion. Shared by
/// `spawn_conduct_tapped` and deterministic tap pins (held-open pipes
/// need no systemd), so the timeout-without-join behavior is itself
/// pinned, not merely reviewed.
pub fn run_tap_drain(
    reader: impl std::io::Read + Send + 'static,
    tail: std::sync::Arc<std::sync::Mutex<TailBuffer>>,
    done: std::sync::mpsc::Sender<DrainOutcome>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        let outcome = loop {
            match reader.read(&mut buf) {
                Ok(0) => break DrainOutcome { error: None },
                Ok(n) => tail.lock().expect("tap mutex").push(&buf[..n]),
                Err(error) => {
                    break DrainOutcome {
                        error: Some(error.to_string()),
                    };
                }
            }
        };
        let _ = done.send(outcome);
    })
}

impl ConductTaps {
    /// Builds taps over an arbitrary reader (deterministic pins; the
    /// live path uses `spawn_conduct_tapped`, which shares
    /// `run_tap_drain`, so pins exercise the production worker).
    pub fn for_reader(reader: impl std::io::Read + Send + 'static) -> Self {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let tail = std::sync::Arc::new(std::sync::Mutex::new(TailBuffer { buf: Vec::new() }));
        let handle = run_tap_drain(reader, std::sync::Arc::clone(&tail), done_tx);
        Self {
            stderr_tail: tail,
            drain_done: done_rx,
            drain_handle: Some(handle),
        }
    }
}

/// Like `spawn_conduct_full`, but conduct's stderr drains into a
/// bounded retained buffer instead of the sink. Stdout still drains to
/// the sink (the minted-id line is consumed at spawn); all other
/// conduct-lifetime semantics are identical.
pub fn spawn_conduct_tapped(
    home: &str,
    profile: &str,
    worktree: &str,
    extra: &[&str],
    argv: &[&str],
    envs: &[(&str, &str)],
    cwd: Option<&std::path::Path>,
) -> (Child, String, Guard, ConductTaps) {
    let mut cmd = Command::new(bin());
    cmd.args([
        "conduct",
        "--profile",
        profile,
        "--session-directory",
        worktree,
    ]);
    cmd.args(extra);
    cmd.arg("--");
    cmd.args(argv);
    cmd.env("HOME", home);
    cmd.env("TERM", "xterm-ghostty");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn conduct");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (id, stdout_rest, leftover) = match read_conduct_id(stdout, CONDUCT_ID_TIMEOUT) {
        Ok(found) => found,
        Err(why) => {
            let _ = child.kill();
            let reaped = reap_bounded(&mut child, REAP_ALLOWANCE).is_some();
            panic!(
                "conduct id line unreadable ({why}; child reaped: {reaped}); conduct stderr: {}",
                drain_stderr(stderr)
            );
        }
    };
    // Blocking restore BEFORE the lifetime drain: the id reader
    // leaves stdout O_NONBLOCK, and the one-shot copy below would
    // exit at the first write gap on a nonblocking fd — dropping
    // the reader and reintroducing the conduct SIGPIPE/141 failure
    // this drain exists to prevent. Restore failure kills loudly.
    if let Err(why) = restore_blocking(&stdout_rest) {
        let _ = child.kill();
        let reaped = reap_bounded(&mut child, REAP_ALLOWANCE).is_some();
        panic!("conduct stdout blocking restore failed ({why}; child reaped: {reaped})");
    }
    // Drain for conduct lifetime: dropping the readers closes the
    // pipes, and conduct's later gate writes would die SIGPIPE
    // (128+13) under main()'s deliberate SigDfl — a racy false
    // failure (proven by QA strace: gate write lands after the
    // drop on loaded seats). Detached drain threads hold the
    // pipes open until EOF at conduct exit; product behavior is
    // untouched (SigDfl hygiene stays).
    //
    // Stderr drains into the bounded retained tap (not the sink) so
    // boundary tests can read conduct's own stderr post-exit; stdout
    // still drains to the sink.
    let (drain_tx, drain_rx) = std::sync::mpsc::channel();
    let mut taps = ConductTaps {
        stderr_tail: std::sync::Arc::new(std::sync::Mutex::new(TailBuffer { buf: Vec::new() })),
        drain_done: drain_rx,
        drain_handle: None,
    };
    let tap_writer = std::sync::Arc::clone(&taps.stderr_tail);
    // Leftover bytes (read past the id-line newline) head the
    // stream so the lifetime drain loses nothing; the reader is
    // blocking again (restored above), so the one-shot copy holds
    // the pipe through write gaps until EOF at conduct exit.
    spawn_stdout_drain(stdout_rest, leftover, std::io::sink());
    // Single drain implementation shared with deterministic pins
    // (`run_tap_drain`): the worker below is the production path the
    // pins exercise.
    let drain_handle = run_tap_drain(stderr, tap_writer, drain_tx);
    taps.drain_handle = Some(drain_handle);
    (child, id.clone(), Guard { id: Some(id) }, taps)
}

/// Best-effort drain of a dead child's pipe for failure diagnostics.
/// Nonblocking via `fcntl` (pipes inherited by grandchildren must never
/// block the panic path). Output is capped as it is read (the cap is a
/// memory bound, not a post-collection truncation); partial reads are
/// fine. Time-bounded by the caller's patience: each empty poll sleeps
/// 50ms against a 2s wall, so a descendant-held fd delays — never
/// hangs — the panic path.
pub fn drain_pipe<R: std::io::Read + std::os::unix::io::AsRawFd>(pipe: R) -> String {
    let mut collected = Vec::new();
    let mut buf = [0u8; 4096];
    let mut pipe = pipe;
    // Nonblocking setup is LOAD-BEARING, not best-effort: without it
    // the reads below can block past the deadline on a held-open
    // pipe. Setup failure returns empty immediately (logged) instead
    // of risking the hang — diagnostics degrade, boundedness holds.
    if let Ok(flags) = nix::fcntl::fcntl(pipe.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL) {
        let mut oflags = nix::fcntl::OFlag::from_bits_retain(flags);
        oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
        if nix::fcntl::fcntl(pipe.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags)).is_err() {
            eprintln!("drain_pipe: O_NONBLOCK setup refused; no diagnostics captured");
            return String::new();
        }
    } else {
        eprintln!("drain_pipe: F_GETFL refused; no diagnostics captured");
        return String::new();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match std::io::Read::read(&mut pipe, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                collected.extend_from_slice(&buf[..n]);
                if collected.len() > 8192 {
                    break;
                }
            }
            Err(_) => {
                if Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    String::from_utf8_lossy(&collected).trim().to_string()
}

/// Best-effort drain of a dead child's stderr for timeout diagnostics.
/// Shared bounded implementation; see `drain_pipe`. Public so
/// failure-path diagnostics (e.g. labels terminate capture) share one
/// bounded drain instead of inventing their own.
pub fn drain_stderr(stderr: std::process::ChildStderr) -> String {
    drain_pipe(stderr)
}

/// Per-poll subprocess budget: each poll invocation composes with
/// the REMAINING outer deadline (never a fresh full budget), capped
/// here so one wedged `systemctl`/`podman` cannot eat the whole
/// wait. A poll that errors (spawn failure, timeout) contributes no
/// observation — the loop continues until the outer deadline, which
/// alone decides failure.
pub const POLL_SUBPROCESS_BUDGET: Duration = Duration::from_secs(10);

/// One poll iteration against the FIXED outer deadline: the
/// iteration may not outlive `outer` (exit + cleanup composed
/// inside it via `run_bounded_at`), and stops consuming the soft
/// per-iteration budget once the outer deadline is near. Returns
/// `None` when the remaining outer budget cannot cover cleanup —
/// the caller then fails with its last observation instead of
/// spawning a poll it cannot reap.
fn poll_at(outer: Instant) -> Option<Instant> {
    let remaining = outer.saturating_duration_since(Instant::now());
    if remaining < REAP_ALLOWANCE + POST_EXIT_DRAIN_TIMEOUT + Duration::from_secs(1) {
        return None;
    }
    Some(
        outer.min(
            Instant::now() + POLL_SUBPROCESS_BUDGET + REAP_ALLOWANCE + POST_EXIT_DRAIN_TIMEOUT,
        ),
    )
}

pub fn wait_active(id: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    // Read on the stop-spawning path (first iteration, no poll
    // yet), overwritten by every poll outcome after.
    let mut last = "no poll completed yet".to_string();
    loop {
        let Some(iter_end) = poll_at(deadline) else {
            panic!("service cistella-{id} never active: {last}");
        };
        let mut cmd = Command::new("systemctl");
        cmd.args([
            "--user",
            "show",
            "-p",
            "ActiveState",
            &format!("cistella-{id}.service"),
        ]);
        match run_bounded_at(cmd, iter_end) {
            Ok(out) if out.status.success() => {
                last = String::from_utf8_lossy(&out.stdout).to_string();
                if last.contains("ActiveState=active") {
                    return;
                }
            }
            // A nonzero poll exit is failed evidence, not state:
            // record stderr, keep polling to the outer deadline.
            Ok(out) => {
                last = format!(
                    "poll exit {}: {}",
                    out.status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".to_string()),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            Err(why) => {
                last = format!("poll failed: {why}");
            }
        }
        if Instant::now() > deadline {
            panic!("service cistella-{id} never active: {last}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Session-up smoke: waits until the harness process is running
/// inside the session container (not just service-active) —
/// `podman top` shows the expected harness ARGV. This attests the
/// container is up and executing the session command; it does NOT
/// itself prove guest-side execution-binding state. The disconnect
/// sweep preserves only bound live executions, so callers that need
/// a bound execution (like the orphan test) state that ordering
/// need themselves — the guest binds the execution on launch, and
/// a visible harness is smoke for a launched session, not a proof
/// of registry state. Matches the harness ARGV (`podman top` shows
/// full command lines): counting processes is insufficient because
/// container PID1 may itself be a sleeper (`podman-init -- sleep
/// infinity` plus a `sleep infinity` child already read as header +
/// two lines pre-harness).
pub fn wait_harness(container: &str, argv: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    // Read on the stop-spawning path (first iteration, no poll
    // yet), overwritten by every poll outcome after.
    let mut last = "no poll completed yet".to_string();
    loop {
        let Some(iter_end) = poll_at(deadline) else {
            panic!("harness {argv} never visible in {container}: {last}");
        };
        let mut cmd = Command::new("podman");
        cmd.args(["top", container]);
        match run_bounded_at(cmd, iter_end) {
            Ok(out) if out.status.success() => {
                last = String::from_utf8_lossy(&out.stdout).to_string();
                if last.contains(argv) {
                    return;
                }
            }
            // A nonzero poll exit is failed evidence, not state:
            // record stderr, keep polling to the outer deadline.
            Ok(out) => {
                last = format!(
                    "poll exit {}: {}",
                    out.status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".to_string()),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            Err(why) => {
                last = format!("poll failed: {why}");
            }
        }
        if Instant::now() > deadline {
            panic!("harness {argv} never visible in {container}: {last}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub fn unit_label(home: &str, id: &str, key: &str) -> String {
    let content = std::fs::read_to_string(unit_path(home, id)).expect("unit readable");
    let prefix = format!("Label={key}=");
    let raw = content
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(|v| v.to_string()))
        .unwrap_or_else(|| panic!("unit missing {key}"));
    // Units store systemd-quoted values; the registry sees the unquoted form.
    cistella::runtime::unquote_systemd(&raw)
}

/// Reads the raw `cistella.directory` label from a unit file path.
pub fn unit_directory_label(path: &PathBuf) -> String {
    let prefix = "Label=cistella.directory=";
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix(prefix).map(|v| v.to_string()))
        .map(|v| cistella::runtime::unquote_systemd(&v))
        .unwrap_or_default()
}

/// Reads one podman label from the running container (single-shot
/// bounded query — a wedged `podman inspect` fails loudly, never hangs).
pub fn podman_label(container: &str, key: &str) -> String {
    let mut cmd = Command::new("podman");
    cmd.args([
        "inspect",
        "--format",
        &format!("{{{{index .Config.Labels \"{key}\"}}}}"),
        container,
    ]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("podman inspect");
    assert!(out.status.success(), "inspect {container}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn wait_for_unit_file(
    unit_dir: &PathBuf,
    before: &std::collections::HashSet<String>,
    worktree: &str,
) -> (String, PathBuf) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(entries) = std::fs::read_dir(unit_dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("cistella-")
                    && name.ends_with(".container")
                    && !before.contains(&name)
                    && unit_directory_label(&e.path()) == worktree
                {
                    let id = name
                        .trim_start_matches("cistella-")
                        .trim_end_matches(".container")
                        .to_string();
                    return (id, e.path());
                }
            }
        }
        if Instant::now() > deadline {
            panic!("unit file never appeared for {worktree}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Session-state enumeration for residue-diff tests: unit files
/// under the shared HOME registry.
pub fn cistella_unit_names(home: &str) -> Vec<String> {
    dir_names_filtered(
        &std::path::PathBuf::from(home).join(".config/containers/systemd"),
        Some("cistella-"),
    )
}

pub fn cistella_tmp_names() -> Vec<String> {
    // XDG entries are bare session ids (plus the lock file itself, stable
    // across the test since the guard is acquired before snapshotting);
    // host /tmp carries the `cistella-` prefix (slices and rendezvous).
    let mut out = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(rt) if !rt.is_empty() => {
            dir_names_filtered(&std::path::PathBuf::from(rt).join("cistella"), None)
        }
        _ => Vec::new(),
    };
    out.extend(dir_names_filtered(
        std::path::Path::new("/tmp"),
        Some("cistella-"),
    ));
    out.sort();
    out
}

fn dir_names_filtered(dir: &std::path::Path, prefix: Option<&str>) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| prefix.is_none_or(|p| n.starts_with(p)))
        .collect();
    names.sort();
    names
}

/// Resolves a built example binary (newest-mtime executable match).
///
/// Requires `cargo build --examples` first for filtered runs
/// (`cargo test --test unit` does not build examples; full
/// `cargo nextest run` does via the default target set).
pub fn example_binary(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt;
    let my_path = std::env::current_exe().expect("current_exe");
    let examples_dir = my_path
        .ancestors()
        .nth(2)
        .expect("target/<profile>/deps ancestors")
        .join("examples");
    let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&examples_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name != name && !file_name.starts_with(&format!("{name}-")) {
                continue;
            }
            let metadata = entry.metadata().expect("metadata");
            if !metadata.is_file() || (metadata.mode() & 0o111) == 0 {
                continue;
            }
            candidates.push((metadata.modified().expect("mtime"), entry.path()));
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    // Prefer the deterministic canonical path (bare name)
    // when present; newest-mtime hashed matches are the
    // fallback for incremental filtered runs.
    let selected = if let Some((_, path)) = candidates
        .iter()
        .find(|(_, path)| path.file_name().is_some_and(|file| file == name))
        .cloned()
    {
        path
    } else {
        candidates
            .into_iter()
            .next()
            .map(|(_, path)| path)
            .unwrap_or_else(|| {
                panic!(
                    "example binary {name} not found in {}; run `cargo build --examples` first",
                    examples_dir.display()
                )
            })
    };
    // Registry verification refuses group/world-writable guest
    // binaries, and cargo honors the host umask (0002 builders
    // emit 775): normalize the peer to 755 at resolution so
    // every consumer conducts a conforming binary.
    std::fs::set_permissions(&selected, std::fs::Permissions::from_mode(0o755))
        .expect("chmod peer binary");
    selected
}

/// Session worktree tmpdir anchored under `~/src`: hooked
/// conducts refuse sessions outside the confinement root, so live
/// fixtures must look like real sessions (`TempDir::new_in`,
/// auto-cleaned, unique per test). Plain sessions behave
/// identically either way, so this helper is uniform.
pub fn src_worktree() -> tempfile::TempDir {
    let root = std::env::var("HOME")
        .map(|home| PathBuf::from(home).join("src"))
        .expect("HOME for live fixtures");
    std::fs::create_dir_all(&root).expect("~/src exists for live fixtures");
    tempfile::TempDir::new_in(&root).expect("src worktree")
}
