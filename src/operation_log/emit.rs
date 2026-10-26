//! Emit helpers: best-effort open, scoped runs, and the
//! per-decision record builders call sites use. Logging failures
//! are silent here by construction; the business operation is
//! unchanged.

use std::path::Path;

use super::{Operation, log_root, read_injected};
use crate::error::Result;

/// Opens one operation log, logging the loss to stderr instead of
/// failing: the business operation always proceeds.
#[must_use]
pub fn open_best_effort(root: &Path, verb: &str, parent: Option<&str>) -> Option<Operation> {
    match Operation::open(root, verb, parent, &read_injected()) {
        Ok(op) => Some(op),
        Err(error) => {
            eprintln!("operation log unavailable ({verb}): {error}");
            None
        }
    }
}

/// Runs one business operation under an operation log: opens the
/// log, runs the body with it, and closes with success or error.
/// Any logging failure is silent; the business result is unchanged.
///
/// # Errors
///
/// Returns the body's error unchanged.
pub fn scoped<T>(
    verb: &str,
    parent: Option<&str>,
    run: impl FnOnce(&mut Option<Operation>) -> Result<T>,
) -> Result<T> {
    scoped_at(&log_root(), verb, parent, run)
}

/// [`scoped`] against an explicit root: the same open, run, and
/// outcome mapping, pinnable without touching process state.
/// Production callers use [`scoped`].
pub fn scoped_at<T>(
    root: &std::path::Path,
    verb: &str,
    parent: Option<&str>,
    run: impl FnOnce(&mut Option<Operation>) -> Result<T>,
) -> Result<T> {
    let mut op = open_best_effort(root, verb, parent);
    let result = run(&mut op);
    // One shared outcome vocabulary: success is exactly
    // "success"; every error closes as "failure" so the pruner
    // files failures under the protected reserve instead of
    // mistaking them for successes.
    if let Some(op) = op {
        op.close(if result.is_ok() { "success" } else { "failure" });
    }
    result
}
/// Truncates one log string with a marker (bounded stream fields).
#[must_use]
pub fn cap_str(raw: &str, cap: usize) -> String {
    if raw.len() <= cap {
        return raw.to_string();
    }
    let mut end = cap.min(raw.len());
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = raw[..end].to_string();
    out.push_str("[truncated]");
    out
}

/// Appends one body record; a logging failure is silent.
pub fn record_step(op: &mut Option<Operation>, body: serde_json::Value) {
    if let Some(op) = op.as_mut() {
        let _ = op.append(&body);
    }
}

/// One gc candidate classification: every field is either observed
/// on the deciding path or an explicit marker. Unobserved fields
/// are never filled by extra backend queries. All strings are owned
/// so callers never stretch a temporary borrow across the call.
///
pub struct Classify {
    /// Container name under classification.
    pub name: String,
    /// Session id as observed (possibly empty).
    pub sid: String,
    /// Whether the inspect probe succeeded.
    pub inspect_ok: bool,
    /// Inspect stderr (capped in the record).
    pub inspect_err: String,
    /// Observed container status, if inspect succeeded.
    pub status: Option<String>,
    /// Container-exists disposition as observed:
    /// `present`, `absent`, or `query-failed`.
    pub exists: String,
    /// Exists-query failure, if the query failed.
    pub exists_err: Option<String>,
    /// Observed unit ActiveState: never queried, queried but
    /// missing, or observed.
    pub unit_active: Observed,
    /// Observed unit LoadState: never queried, queried but
    /// missing, or observed.
    pub unit_load: Observed,
    /// Classification decision.
    pub decision: String,
}

/// A unit property as observed, distinguishing three cases the
/// classifier must not conflate: never queried on this path,
/// queried but missing (absent key or empty value), or observed
/// with a value. Rendered as `not-queried`, `unavailable`, or the
/// value itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// The scan never read this property on this path.
    NotQueried,
    /// The scan read for it, but it was absent or empty.
    Missing,
    /// The scan observed this value.
    Present(String),
}

impl Observed {
    /// The record string for one observed property.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Observed::NotQueried => "not-queried",
            Observed::Missing => "unavailable",
            Observed::Present(value) => value.as_str(),
        }
    }

    /// Maps one queried property to its observed state: a missing
    /// key or an empty value reads as missing, never as a default.
    #[must_use]
    pub fn queried(value: Option<&str>) -> Self {
        match value {
            Some(text) if !text.is_empty() => Observed::Present(text.to_string()),
            _ => Observed::Missing,
        }
    }
}

/// Records one gc candidate classification: what the scan observed
/// (never more); silent on logging failure. Fields the scan never
/// reads carry explicit markers instead of invented values: the
/// unit identity is not observed by the classifier, the full
/// container id is not queried, and neither the attempt nor the
/// reconciliation key is read.
pub fn record_gc_classify(op: &mut Option<Operation>, c: &Classify) {
    record_step(
        op,
        serde_json::json!({
            "phase": "classify",
            "name": c.name,
            "sid": c.sid,
            "inspect_ok": c.inspect_ok,
            "inspect_err": cap_str(&c.inspect_err, 500),
            "status": c.status,
            "exists": c.exists,
            "exists_error": c.exists_err.as_deref().unwrap_or("none"),
            "unit_active": c.unit_active.as_str(),
            "unit_load": c.unit_load.as_str(),
            "invocation_id": "not-queried",
            "container_id": "unavailable",
            "attempt": "not-queried",
            "reconciliation_key": "not-queried",
            "decision": c.decision,
        }),
    );
}

/// Records a gc scan-level receipt (used when the scan aborts
/// before any per-candidate classification exists).
pub fn record_gc_scan(op: &mut Option<Operation>, decision: &str, detail: &str) {
    record_step(
        op,
        serde_json::json!({
            "phase": "scan",
            "decision": decision,
            "detail": cap_str(detail, 500),
        }),
    );
}

/// Records one gc teardown outcome; silent on logging failure.
pub fn record_gc_reap(
    op: &mut Option<Operation>,
    name: &str,
    sid: &str,
    orphan_unit: bool,
    rm_ok: bool,
    rm_stderr: &str,
) {
    record_step(
        op,
        serde_json::json!({
            "phase": "reap",
            "name": name,
            "sid": sid,
            "orphan_unit": orphan_unit,
            "rm_ok": rm_ok,
            "rm_stderr": cap_str(rm_stderr, 500),
        }),
    );
}

/// Records one gc sweep phase (tmpdir, attestation); silent on
/// logging failure.
pub fn record_gc_phase(op: &mut Option<Operation>, phase: &str, reaped: &[String]) {
    record_step(
        op,
        serde_json::json!({
            "phase": phase,
            "reaped": reaped,
        }),
    );
}
