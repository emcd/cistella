//! Quadlet orphan reaping: the global gc scan.
//!
//! Scan-and-teardown under the held creation-window lock, with
//! per-target operation-log records. Extracted from `quadlet.rs`
//! (unit mechanics stay there); the public paths are unchanged.

use std::process::Command;

use crate::error::{CistellaError, Result};
use crate::isolators::quadlet::{
    container_exists, quadlet_dir, query_unit_props, systemd_user_available,
    teardown_inner_with_parent,
};
use crate::lock::LockGuard;
use crate::operation_log;
use crate::registry::unit_file_label;
use crate::session::LABEL_ID;

/// GC result.
#[derive(Debug, Clone)]
pub struct GcResult {
    /// Reaped container names plus removed orphaned slice dirs.
    pub reaped: Vec<String>,
}

/// How one candidate's scan ends: keep classifying, or stop the
/// whole scan. The abort variants carry no decision inputs; their
/// receipts still record the observed values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortKind {
    /// The exists query itself failed: nothing is known.
    QueryFailed,
    /// The container exists but is uninspectable: fail closed.
    FailClosed,
}

/// One candidate's classification: the decision inputs plus the
/// observed values exactly as they go into the receipt. Produced by
/// [`classify_candidate`], the gc's only classification path, so
/// unit tests driving that function pin the production recording.
#[derive(Debug, Clone)]
pub struct CandidateClassification {
    /// Inspect saw an exited container.
    pub is_exited_container: bool,
    /// The unit is absent from podman with an inactive, failed, or
    /// not-found unit behind it.
    pub is_orphan_unit: bool,
    /// Container-exists disposition: `present`, `absent`, or
    /// `query-failed`.
    pub exists: &'static str,
    /// Exists-query failure, if the query failed.
    pub exists_err: Option<String>,
    /// Observed unit ActiveState: never queried, queried but
    /// missing, or observed.
    pub unit_active: operation_log::Observed,
    /// Observed unit LoadState: never queried, queried but missing,
    /// or observed.
    pub unit_load: operation_log::Observed,
    /// Classification decision.
    pub decision: &'static str,
    /// Set on the two paths where the gc stops the whole scan
    /// instead of continuing to the next candidate.
    pub abort: Option<AbortKind>,
}

/// Classifies one gc candidate from its raw observations. Pure: no
/// I/O, no podman, no filesystem — the gc reads everything first
/// and passes it here, then records exactly what comes back.
/// `status` is the inspect status when inspect succeeded; `exists`
/// is the `container exists` result (`Err` carries the query error
/// text); `unit_props` is `Some` only on the path that actually
/// read the unit; `sid` is the session id known so far.
#[must_use]
pub fn classify_candidate(
    status: Option<&str>,
    exists: std::result::Result<bool, String>,
    unit_props: Option<&std::collections::HashMap<String, String>>,
    sid: &str,
) -> CandidateClassification {
    match status {
        Some(status) => {
            let exited = status == "exited";
            CandidateClassification {
                is_exited_container: exited,
                is_orphan_unit: false,
                exists: "present",
                exists_err: None,
                unit_active: operation_log::Observed::NotQueried,
                unit_load: operation_log::Observed::NotQueried,
                decision: if exited {
                    "reap-exited-container"
                } else if sid.is_empty() {
                    "unresolved-no-session"
                } else {
                    "live"
                },
                abort: None,
            }
        }
        None => match exists {
            Err(error) => CandidateClassification {
                is_exited_container: false,
                is_orphan_unit: false,
                exists: "query-failed",
                exists_err: Some(error),
                unit_active: operation_log::Observed::NotQueried,
                unit_load: operation_log::Observed::NotQueried,
                decision: "aborted-query-failed",
                abort: Some(AbortKind::QueryFailed),
            },
            Ok(true) => CandidateClassification {
                is_exited_container: false,
                is_orphan_unit: false,
                exists: "present",
                exists_err: None,
                unit_active: operation_log::Observed::NotQueried,
                unit_load: operation_log::Observed::NotQueried,
                decision: "aborted-fail-closed",
                abort: Some(AbortKind::FailClosed),
            },
            Ok(false) => {
                let mut orphan = false;
                let mut unit_active = operation_log::Observed::NotQueried;
                let mut unit_load = operation_log::Observed::NotQueried;
                if let Some(props) = unit_props {
                    // Recorded exactly as queried: a missing key or
                    // an empty value is Missing, never the decision
                    // default below.
                    unit_active = operation_log::Observed::queried(
                        props.get("ActiveState").map(String::as_str),
                    );
                    unit_load = operation_log::Observed::queried(
                        props.get("LoadState").map(String::as_str),
                    );
                    // The decision keeps its historical decision
                    // defaults; only the record distinguishes missing
                    // from observed.
                    let active = props
                        .get("ActiveState")
                        .cloned()
                        .unwrap_or_else(|| "inactive".to_string());
                    let load = props
                        .get("LoadState")
                        .cloned()
                        .unwrap_or_else(|| "not-found".to_string());
                    if load == "not-found" || active == "inactive" || active == "failed" {
                        orphan = true;
                    }
                }
                CandidateClassification {
                    is_exited_container: false,
                    is_orphan_unit: orphan,
                    exists: "absent",
                    exists_err: None,
                    unit_active,
                    unit_load,
                    decision: if orphan {
                        "reap-orphan-unit"
                    } else if sid.is_empty() {
                        "unresolved-no-session"
                    } else {
                        "live"
                    },
                    abort: None,
                }
            }
        },
    }
}

/// Reaps orphaned cistella units and stray non-systemd containers.
///
/// Under Quadlet `--rm` a stopped/crashed service has no `podman ps` entry,
/// so GC enumerates BOTH `~/.config/containers/systemd/cistella-*.container`
/// files and `podman ps --all` `cistella.id` labels. For each candidate: if
/// `ActiveState` is inactive/failed/not-found the shared teardown runs;
/// active units are left. Holds the creation-window lock around the whole
/// scan-and-teardown so a concurrent `conduct` install is never reaped.
/// Fail-closed on any inspect error.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if podman list fails; on inspect error
/// returns empty Ok (fail-closed).
pub fn gc_exited() -> Result<GcResult> {
    let _guard = LockGuard::acquire()?;
    gc_exited_locked()
}

/// Lock-held half of `gc_exited` for the creation-window test, which holds
/// the guard to prove a concurrent `gc` reaps nothing.
///
/// Opens the gc operation log; the inner scan classifies and the
/// outer close records success or error, so every exit path leaves
/// an outcome. Logging failures never change the decision.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` if podman list fails.
pub fn gc_exited_locked() -> Result<GcResult> {
    operation_log::scoped("gc", None, gc_exited_locked_inner)
}

/// Lists gc candidates from `program ps --all` (production:
/// `podman`). The program path is a parameter so tests drive the
/// spawn-failure path with a nonexistent executable without any
/// backend: a spawn failure records the abort scan receipt with
/// the actual invocation error before propagating it unchanged,
/// exactly as a spawned-but-failed run does.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the listing cannot be
/// spawned or exits nonzero; the abort receipt is recorded first
/// in both cases.
pub fn ps_candidate_names(
    op: &mut Option<operation_log::Operation>,
    program: &str,
) -> Result<std::collections::HashSet<String>> {
    let out = match Command::new(program)
        .args([
            "ps",
            "--all",
            "--filter",
            "label=cistella.id",
            "--format",
            "{{.Names}}",
        ])
        .output()
    {
        Ok(out) => out,
        Err(error) => {
            let detail = format!("podman ps: {error}");
            operation_log::record_gc_scan(op, "aborted-ps-failed", &detail);
            return Err(CistellaError::Runtime(detail));
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        operation_log::record_gc_scan(op, "aborted-ps-failed", &stderr);
        return Err(CistellaError::Runtime(format!("ps failed: {stderr}")));
    }
    let mut candidates = std::collections::HashSet::new();
    for n in String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        candidates.insert(n);
    }
    Ok(candidates)
}

/// Scans and reaps under the held creation lock. Records what the
/// scan observed (never more); any logging failure is silent.
fn gc_exited_locked_inner(op: &mut Option<operation_log::Operation>) -> Result<GcResult> {
    let mut reaped = Vec::new();
    let systemd = systemd_user_available();
    // Enumerate candidates from both sources: unit files + podman ps
    let mut candidates = ps_candidate_names(op, "podman")?;
    // From Quadlet unit directory (orphaned files invisible to ps due to --rm)
    if let Some(dir) = quadlet_dir()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        for e in entries.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            if fname.starts_with("cistella-") && fname.ends_with(".container") {
                let cname = fname.trim_end_matches(".container").to_string();
                candidates.insert(cname);
            }
        }
    }
    // Phase 1: classify every candidate before mutating anything. Any
    // inspect/exists failure aborts here with zero teardowns, so "fail
    // closed on any inspect error" holds regardless of HashSet order.
    // Each entry is (container name, session id, reap-as-orphan-unit). The
    // session id comes from the SAME inspect call as the status (plus the
    // unit file for absent containers): no second inspect whose failure
    // could be silently swallowed.
    let mut to_reap: Vec<(String, String, bool)> = Vec::new();
    // Session ids of units still present after this run (running
    // or otherwise not selected): tmpdir-orphan eligibility skips
    // them. A failed unit scan aborts the whole gc before any
    // mutation (above), so this set never reads failure as
    // absence — failed scan is not "no unit".
    let mut live_sessions: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ordered: Vec<String> = candidates.into_iter().collect();
    ordered.sort();
    for name in ordered {
        // Absence primitive is `podman container exists`: exit 0 present,
        // exit 1 absent, anything else a typed error that reaps nothing.
        // A failed `inspect` alone must never read as absence (fail closed).
        let insp = Command::new("podman")
            .args([
                "inspect",
                "--format",
                "{{.State.Status}} {{index .Config.Labels \"cistella.id\"}}",
                &name,
            ])
            .output();
        let (inspected, inspect_err) = match insp {
            Ok(o) if o.status.success() => (
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
                String::new(),
            ),
            Ok(o) => (None, String::from_utf8_lossy(&o.stderr).to_string()),
            Err(e) => (None, e.to_string()),
        };
        // Status and id ride one call: `<status> <id>`, with `<no value>`
        // (whitespace-split) when the container carries no `cistella.id`
        // label. Minted ids never contain whitespace, so parts[1] alone is
        // exact; anything starting with `<` is podman's missing marker.
        let (status, mut sid) = match inspected.as_deref() {
            Some(out) => {
                let parts: Vec<&str> = out.split_whitespace().collect();
                let status = parts.first().unwrap_or(&"").to_string();
                let sid = match parts.get(1) {
                    Some(id) if !id.starts_with('<') => id.to_string(),
                    _ => String::new(),
                };
                (Some(status), sid)
            }
            None => (None, String::new()),
        };
        // Reads for this candidate: the present-container path never
        // queries the unit; the absent-container path reads the unit
        // file (for the session id) and the unit properties only
        // when a unit file exists. Classification itself is the pure
        // `classify_candidate` mapping, recorded exactly as returned.
        let mut unit_props: Option<std::collections::HashMap<String, String>> = None;
        let exists_result: std::result::Result<bool, String> = match status.as_deref() {
            Some(_) => Ok(true),
            None => match container_exists(&name) {
                Ok(present) => {
                    if !present {
                        // Expected absence under Quadlet --rm: a unit file behind
                        // it means orphan candidacy. The session id comes from
                        // the unit file (pure fs read, no podman): the only
                        // source for `--rm`'d orphans.
                        if let Some(dir) = quadlet_dir() {
                            let path = dir.join(format!("{name}.container"));
                            if path.exists() {
                                if sid.is_empty() {
                                    sid = unit_file_label(&path, LABEL_ID)
                                        .ok()
                                        .flatten()
                                        .unwrap_or_default();
                                }
                                if systemd {
                                    let service = format!("{name}.service");
                                    unit_props = Some(query_unit_props(&service));
                                }
                            }
                        }
                    }
                    Ok(present)
                }
                Err(error) => {
                    let classification =
                        classify_candidate(None, Err(error.to_string()), None, &sid);
                    operation_log::record_gc_classify(
                        op,
                        &operation_log::Classify {
                            name: name.clone(),
                            sid: sid.clone(),
                            inspect_ok: false,
                            inspect_err: inspect_err.clone(),
                            status: None,
                            exists: classification.exists.to_string(),
                            exists_err: classification.exists_err,
                            unit_active: classification.unit_active,
                            unit_load: classification.unit_load,
                            decision: classification.decision.to_string(),
                        },
                    );
                    return Err(error);
                }
            },
        };
        let classification =
            classify_candidate(status.as_deref(), exists_result, unit_props.as_ref(), &sid);
        if classification.abort == Some(AbortKind::FailClosed) {
            // Present but uninspectable: a real invocation failure,
            // not Quadlet `--rm` absence. Fail closed, with the
            // receipt written before the abort.
            operation_log::record_gc_classify(
                op,
                &operation_log::Classify {
                    name: name.clone(),
                    sid: sid.clone(),
                    inspect_ok: false,
                    inspect_err: inspect_err.clone(),
                    status: None,
                    exists: classification.exists.to_string(),
                    exists_err: classification.exists_err,
                    unit_active: classification.unit_active,
                    unit_load: classification.unit_load,
                    decision: classification.decision.to_string(),
                },
            );
            return Err(CistellaError::Runtime(format!(
                "podman inspect {name} failed while container exists: {inspect_err}"
            )));
        }
        if classification.is_exited_container || classification.is_orphan_unit {
            to_reap.push((name.clone(), sid.clone(), classification.is_orphan_unit));
        } else if !sid.is_empty() {
            live_sessions.insert(sid.clone());
        }
        operation_log::record_gc_classify(
            op,
            &operation_log::Classify {
                name: name.clone(),
                sid: sid.clone(),
                inspect_ok: inspected.is_some(),
                inspect_err: inspect_err.clone(),
                status: status.clone(),
                exists: classification.exists.to_string(),
                exists_err: classification.exists_err,
                unit_active: classification.unit_active,
                unit_load: classification.unit_load,
                decision: classification.decision.to_string(),
            },
        );
    }
    // Phase 2: teardown only classified orphans. A teardown failure still
    // propagates, but every candidate was classified before the first
    // mutation, so no inspect error can follow a deletion.
    // The gc operation id, borrowed for the child teardown's
    // parent link. Recomputed per iteration because `op` moves
    // across the scoped call boundary below.
    for (name, sid, is_orphan_unit) in to_reap {
        if systemd {
            let parent = op.as_ref().map(|op| op.id().to_string());
            teardown_inner_with_parent(&name, &sid, parent.as_deref())?;
        }
        // Try podman rm for stray non-systemd containers; for Quadlet orphans there is no container so this is no-op.
        let rm = Command::new("podman").args(["rm", "-f", &name]).output();
        let rm_ok = matches!(&rm, Ok(output) if output.status.success());
        if rm_ok || is_orphan_unit {
            reaped.push(name.clone());
        }
        operation_log::record_gc_reap(
            op,
            &name,
            &sid,
            is_orphan_unit,
            rm_ok,
            &rm.map(|output| String::from_utf8_lossy(&output.stderr).to_string())
                .unwrap_or_default(),
        );
    }
    // Phase 3: tmpdir orphans attributable to dead sessions. Unit
    // teardowns above converged (any failure propagated instead),
    // so sessions absent from the live set are dead — orphans with
    // live leases or live guest groups still skip inside.
    let tmpdir_reaped = crate::cleanup::reap_tmpdir_orphans(&live_sessions);
    operation_log::record_gc_phase(op, "tmpdir", &tmpdir_reaped);
    reaped.extend(tmpdir_reaped);
    // Phase 3b: attestation orphans (sessions with no live unit:
    // crashed conducts, pre-teardown kills). Same live-set gate;
    // reaped names report alongside containers.
    let attestation_reaped =
        crate::attestation::reap_orphans(&crate::lock::runtime_base(), &live_sessions);
    operation_log::record_gc_phase(op, "attestation", &attestation_reaped);
    reaped.extend(attestation_reaped);
    Ok(GcResult { reaped })
}
