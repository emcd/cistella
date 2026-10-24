//! CLI selector utilities for the `cistella` binary: label-filter
//! parsing, exact session resolution, directory canonicalization.
//!
//! Bin-crate module (declared in `main.rs`, NOT part of the library):
//! moved verbatim from `main.rs` along verb seams, no logic changes.

use cistella::framework::isolator::Isolator;
use cistella::isolators::podman::PodmanIsolator;
use cistella::registry::{list_sessions, resolve_exact};
use cistella::session::parse_cli_label;

/// Parses and validates `--label k=v` filters (refuses `cistella.`).
pub(crate) fn parse_label_filters(
    labels: &[String],
) -> Result<Vec<(String, String)>, cistella::error::CistellaError> {
    labels.iter().map(|a| parse_cli_label(a)).collect()
}

/// Resolves exactly one selector against the live registry.
pub(crate) fn select_exact(
    id: Option<&str>,
    directory: Option<&str>,
    labels: &[String],
) -> Result<cistella::registry::SessionRecord, cistella::error::CistellaError> {
    let records = list_sessions()?;
    let parsed = parse_label_filters(labels)?;
    let canon_dir = directory.map(canonical_directory).transpose()?;
    resolve_exact(&records, id, canon_dir.as_deref(), &parsed)
}

/// Canonicalizes a host directory via longest existing prefix.
///
/// `--session-directory` host defaults to the caller's cwd; both conduct-time and
/// selector-time canonicalize the same way so equality matches.
pub(crate) fn canonical_directory(dir: &str) -> Result<String, cistella::error::CistellaError> {
    if !dir.starts_with('/') {
        return Err(cistella::error::CistellaError::Runtime(format!(
            "directory must be absolute: {dir}"
        )));
    }
    Ok(cistella::mount::canonicalize_host_source(dir)
        .to_string_lossy()
        .to_string())
}

/// Reports launch commitment plus backend snapshot for one
/// selector (`describe state`). Ownerless by construction: a
/// fresh backend holds no executions, so the snapshot never
/// claims `executing`. Resolution mirrors `select_exact`
/// exactly; only a full-shape id with zero record matches
/// falls to the derived-name probe (prefix/directory/label
/// no-match and ambiguity keep their canonical errors).
pub(crate) fn describe_state(
    id: Option<&str>,
    directory: Option<&str>,
    labels: &[String],
) -> Result<(), cistella::error::CistellaError> {
    match select_exact(id, directory, labels) {
        Ok(record) => describe_record(&record),
        Err(error) => {
            let Some(prefix) = id else {
                return Err(error);
            };
            if directory.is_some() || !labels.is_empty() {
                return Err(error);
            }
            if !cistella::describe::full_id_shape(prefix) {
                return Err(error);
            }
            // Recount from a fresh listing to separate true
            // no-match (probe) from ambiguity (canonical error)
            // without matching on error text.
            let records = list_sessions()?;
            let count = records
                .iter()
                .filter(|record| record.id.starts_with(prefix))
                .count();
            if count == 0 {
                describe_probe(prefix)
            } else {
                Err(error)
            }
        }
    }
}

/// Renders one record-known session: commitment bit plus
/// adopted-backend snapshot (or the foreign shape for
/// unincarnated commitments).
fn describe_record(
    record: &cistella::registry::SessionRecord,
) -> Result<(), cistella::error::CistellaError> {
    let base = cistella::lock::runtime_base();
    let attestation = cistella::attestation::read(&base, &record.id)?;
    let Some(bound_attestation) = attestation.as_ref() else {
        // No commitment: the backend is unproven — explicitly
        // uncertain with no snapshot. A record alone proves
        // nothing about which backend, if any, is live, so no
        // backend is claimed and no lifecycle is invented.
        println!(
            "{} commitment=unattested backend=unknown",
            record.container_name,
        );
        return Ok(());
    };
    // Container binding before anything else: the attestation
    // must bind THIS record's container (tamper or out-of-band
    // recreate), or every later fact is attributed wrongly.
    if bound_attestation.container_name != record.container_name {
        return Err(cistella::error::CistellaError::Runtime(format!(
            "describe state: attestation container mismatch for {} (possible out-of-band recreate)",
            record.container_name,
        )));
    }
    let Some(bound) = bound_attestation.incarnation.as_ref() else {
        // Contradictory provenance: an unincarnated (foreign)
        // commitment for a record-holding session cannot occur
        // in production (foreign conducts create no unit
        // files), so fail closed loud instead of rendering.
        return Err(cistella::error::CistellaError::Runtime(format!(
            "describe state: foreign commitment for native record {} (contradictory provenance)",
            record.container_name,
        )));
    };
    let backend = PodmanIsolator::new();
    let handle = backend.adopt(&record.container_name, &record.id);
    let snapshot = backend.inspect(&handle)?;
    let state = cistella::describe::snapshot_checked(&snapshot)?;
    let rendered = cistella::describe::render_state(&state);
    let incarnation = match cistella::attestation::query_incarnation(&record.container_name) {
        Ok(live) if &live == bound => "current",
        Ok(_) => "stale",
        Err(_) => "unknown",
    };
    println!(
        "{container} state={rendered} commitment=attested backend=native incarnation={incarnation}",
        container = record.container_name,
    );
    Ok(())
}

/// Resolves a full id with no record via the derived container
/// name: live backend residue errors typed (never `absent`);
/// inert attestation residue and all-absent resolve `absent`.
fn describe_probe(id: &str) -> Result<(), cistella::error::CistellaError> {
    let container_name = format!("cistella-{id}");
    let evidence = cistella::describe::probe_native(&container_name)?;
    let attestation = cistella::attestation::read(&cistella::lock::runtime_base(), id)?;
    // Unincarnated (foreign) commitment with no local residue:
    // foreign absence cannot be established from local Podman
    // evidence — unsupported, never an inferred `absent`.
    // Local residue still dominates (backend fact first).
    if attestation
        .as_ref()
        .is_some_and(|attested| attested.incarnation.is_none())
        && !evidence.container_present
        && !evidence.unit_present
    {
        return Err(cistella::error::CistellaError::Runtime(
            "describe state: foreign session identity not establishable from local evidence (unsupported)".to_string(),
        ));
    }
    match cistella::describe::probe_table(&container_name, &evidence, attestation.is_some()) {
        cistella::describe::ProbeOutcome::Residue { container_name } => {
            Err(cistella::error::CistellaError::Runtime(format!(
                "describe state: residue {container_name} present with no record; terminate or gc to converge"
            )))
        }
        cistella::describe::ProbeOutcome::AbsentAttested => {
            // Scope, not backend: the query probed the native
            // backend and found inert commitment residue there.
            // No claim about any other backend is made.
            println!("{container_name} state=absent commitment=attested scope=native");
            Ok(())
        }
        cistella::describe::ProbeOutcome::AbsentClean => {
            // Scope, not backend: absent from the probed native
            // backend. A session unknown anywhere else is not
            // distinguished from never-existing — the scope
            // marker is the honesty mechanism.
            println!("{container_name} state=absent commitment=unattested scope=native");
            Ok(())
        }
    }
}

/// Single-pass enter readiness edge for one resolved record:
/// commitment + incarnation + native admittability, refusing
/// typed on the first unmet condition. No retries, no polls.
/// Foreign (unincarnated) commitments route to the legacy
/// direct exec with no gate and no readiness claim.
pub(crate) fn gate_enter(
    record: &cistella::registry::SessionRecord,
) -> Result<cistella::describe::EnterDecision, cistella::error::CistellaError> {
    let base = cistella::lock::runtime_base();
    gate_enter_with(
        record,
        &|id| cistella::attestation::read(&base, id),
        &cistella::attestation::query_incarnation,
        &cistella::describe::exec_admittability,
    )
}

/// Injection seam behind `gate_enter`: the same
/// read-compare-probe-decide wiring with the three contact
/// steps supplied by the caller. Production passes the live
/// queries; pins inject fabricated outcomes. No retries, no
/// polls — one pass over one record, exactly like `gate_enter`.
fn gate_enter_with(
    record: &cistella::registry::SessionRecord,
    read_attestation: &dyn Fn(
        &str,
    ) -> Result<
        Option<cistella::attestation::Attestation>,
        cistella::error::CistellaError,
    >,
    query_identity: &dyn Fn(
        &str,
    ) -> Result<
        cistella::attestation::Incarnation,
        cistella::error::CistellaError,
    >,
    probe_admittability: &dyn Fn(
        &str,
    ) -> Result<
        cistella::describe::Admittability,
        cistella::error::CistellaError,
    >,
) -> Result<cistella::describe::EnterDecision, cistella::error::CistellaError> {
    use cistella::describe::{EnterDecision, LiveIncarnation, decide_enter};
    let attestation = read_attestation(&record.id)?;
    // Unattested sessions refuse before any backend contact:
    // there is no commitment whose admittability could be
    // checked, and the refusal must not depend on podman.
    // Unincarnated (foreign) commitments route legacy-direct
    // with no gate and no claim, likewise contact-free.
    let Some(bound) = attestation.as_ref().and_then(|a| a.incarnation.as_ref()) else {
        return Ok(match attestation.as_ref() {
            None => EnterDecision::Refuse("no launch attestation committed".to_string()),
            Some(_) => EnterDecision::ProceedLegacyDirect,
        });
    };
    // Bound session: compare live incarnation, then gather
    // admittability. Backend query failures error typed (never
    // silent absence); unknown identity refuses, never proceeds.
    let live = match query_identity(&record.container_name) {
        Ok(observed) if &observed == bound => LiveIncarnation::Current,
        Ok(_) => LiveIncarnation::Stale,
        Err(_) => LiveIncarnation::Unknown,
    };
    let admittability = probe_admittability(&record.container_name)?;
    Ok(decide_enter(
        attestation.as_ref(),
        &record.container_name,
        &live,
        &admittability,
    ))
}

#[cfg(test)]
mod gate_tests {
    // Inline by exception (project rule): the gate wiring
    // lives in the bin crate by design, and no public
    // interface reaches its read-compare-probe-decide path
    // without a live backend. Single test: the three
    // companion controls over the injected seam — a supplied
    // current identity proceeds, a supplied different
    // identity forces the stale refusal (ruling out an
    // always-Unknown wiring), an unsuppliable identity
    // forces the unknown refusal. This proves the wiring
    // maps supplied identities to decisions; the live arm
    // proves the CLI's own query reaches it.
    use super::gate_enter_with;
    use cistella::attestation::{Attestation, Incarnation, parse_identity};
    use cistella::describe::{Admittability, EnterDecision};

    fn record() -> cistella::registry::SessionRecord {
        cistella::registry::SessionRecord {
            id: "s-1".to_string(),
            container_name: "cistella-s-1".to_string(),
            directory: "/work".to_string(),
            profile: "default".to_string(),
            identity: "alice".to_string(),
            image: "image".to_string(),
            active_state: "active".to_string(),
            container_present: true,
            generic_labels: Vec::new(),
            hooked: Some(false),
        }
    }

    fn attested(incarnation: Option<Incarnation>) -> Attestation {
        Attestation {
            session_id: "s-1".to_string(),
            container_name: "cistella-s-1".to_string(),
            attempt_key: "key-1".to_string(),
            argv: vec!["sleep".to_string()],
            execution_handle: "exec-1".to_string(),
            incarnation,
        }
    }

    fn admitted() -> Admittability {
        Admittability {
            running: true,
            init_pid: 1,
            unit_present: true,
        }
    }

    #[test]
    fn injected_identity_drives_the_gate_decision() {
        let bound = Incarnation {
            container_id: "id-1".to_string(),
            started_at: "t-1".to_string(),
        };
        let record = record();
        let read = |_: &str| {
            Ok(Some(attested(Some(bound.clone()))))
                as Result<Option<Attestation>, cistella::error::CistellaError>
        };
        let probe =
            |_: &str| Ok(admitted()) as Result<Admittability, cistella::error::CistellaError>;
        // Supplied current identity proceeds native.
        let query =
            |_: &str| Ok(bound.clone()) as Result<Incarnation, cistella::error::CistellaError>;
        assert!(
            matches!(
                gate_enter_with(&record, &read, &query, &probe),
                Ok(EnterDecision::ProceedNative)
            ),
            "supplied current identity proceeds"
        );
        // Supplied different identity forces the stale refusal.
        let changed = Incarnation {
            container_id: "id-1".to_string(),
            started_at: "t-2".to_string(),
        };
        let query =
            |_: &str| Ok(changed.clone()) as Result<Incarnation, cistella::error::CistellaError>;
        match gate_enter_with(&record, &read, &query, &probe) {
            Ok(EnterDecision::Refuse(why)) if why.contains("stale") => {}
            outcome => panic!("supplied different identity must refuse stale, got: {outcome:?}"),
        }
        // Unsuppliable identity forces the unknown refusal.
        let query = |_: &str| {
            Err(cistella::error::CistellaError::Runtime(
                "contact failed".to_string(),
            )) as Result<Incarnation, cistella::error::CistellaError>
        };
        match gate_enter_with(&record, &read, &query, &probe) {
            Ok(EnterDecision::Refuse(why)) if why.contains("unestablishable") => {}
            outcome => {
                panic!("unsuppliable identity must refuse unestablishable, got: {outcome:?}")
            }
        }
        // Direct regression pin for the truncation defect: two
        // full renderings with the same date but different
        // times — the committed bound and the supplied live
        // identity — must refuse stale. The old second-token
        // parse collapsed both to the same date and proceeded.
        let bound_rendering =
            parse_identity("id-9 2026-10-24 14:51:00 +0000 UTC").expect("bound rendering parses");
        let live_rendering =
            parse_identity("id-9 2026-10-24 15:06:00 +0000 UTC").expect("live rendering parses");
        assert_ne!(
            bound_rendering, live_rendering,
            "renderings with different times stay distinct"
        );
        let read = |_: &str| {
            Ok(Some(attested(Some(bound_rendering.clone()))))
                as Result<Option<Attestation>, cistella::error::CistellaError>
        };
        let query = |_: &str| {
            Ok(live_rendering.clone()) as Result<Incarnation, cistella::error::CistellaError>
        };
        match gate_enter_with(&record, &read, &query, &probe) {
            Ok(EnterDecision::Refuse(why)) if why.contains("stale") => {}
            outcome => panic!("same-date restart must refuse stale, got: {outcome:?}"),
        }
        // Old-marker control (named separately): a date-only
        // marker from a lossy old writer, read by the fixed
        // reader against a full live rendering, refuses
        // conservative (stale) — a liveness cost, not a safety
        // hole.
        let old_marker = parse_identity("id-9 2026-10-24").expect("date-only marker parses");
        let read = |_: &str| {
            Ok(Some(attested(Some(old_marker.clone()))))
                as Result<Option<Attestation>, cistella::error::CistellaError>
        };
        match gate_enter_with(&record, &read, &query, &probe) {
            Ok(EnterDecision::Refuse(why)) if why.contains("stale") => {}
            outcome => panic!("lossy old marker must refuse stale, got: {outcome:?}"),
        }
    }
}
