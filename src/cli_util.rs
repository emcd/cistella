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
    let incarnated = attestation
        .as_ref()
        .and_then(|attested| attested.incarnation.as_ref());
    if attestation.is_some() && incarnated.is_none() {
        // Foreign shape: commitment plus verbatim record
        // metadata plus explicit unsupported admittability —
        // never an invented lifecycle snapshot.
        println!(
            "{} state=unavailable commitment=attested backend=foreign admittability=unsupported profile={} directory={} identity={}",
            record.container_name, record.profile, record.directory, record.identity,
        );
        return Ok(());
    }
    let backend = PodmanIsolator::new();
    let handle = backend.adopt(&record.container_name, &record.id);
    let snapshot = backend.inspect(&handle)?;
    let state = cistella::describe::snapshot_checked(&snapshot)?;
    let rendered = cistella::describe::render_state(&state);
    match incarnated {
        None => println!(
            "{} state={rendered} commitment=unattested backend=native",
            record.container_name,
        ),
        Some(bound) => {
            let incarnation = match cistella::attestation::query_incarnation(&record.container_name)
            {
                Ok(live) if &live == bound => "current",
                Ok(_) => "stale",
                Err(_) => "unknown",
            };
            println!(
                "{} state={rendered} commitment=attested backend=native incarnation={incarnation}",
                record.container_name,
            );
        }
    }
    Ok(())
}

/// Resolves a full id with no record via the derived container
/// name: live backend residue errors typed (never `absent`);
/// inert attestation residue and all-absent resolve `absent`.
fn describe_probe(id: &str) -> Result<(), cistella::error::CistellaError> {
    let container_name = format!("cistella-{id}");
    let evidence = cistella::describe::probe_native(&container_name)?;
    let attestation = cistella::attestation::read(&cistella::lock::runtime_base(), id)?.is_some();
    match cistella::describe::probe_table(&container_name, &evidence, attestation) {
        cistella::describe::ProbeOutcome::Residue { container_name } => {
            Err(cistella::error::CistellaError::Runtime(format!(
                "describe state: residue {container_name} present with no record; terminate or gc to converge"
            )))
        }
        cistella::describe::ProbeOutcome::AbsentAttested => {
            println!("{container_name} state=absent commitment=attested backend=native");
            Ok(())
        }
        cistella::describe::ProbeOutcome::AbsentClean => {
            println!("{container_name} state=absent commitment=unattested backend=native");
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
    use cistella::describe::{EnterDecision, LiveIncarnation, decide_enter};
    let base = cistella::lock::runtime_base();
    let attestation = cistella::attestation::read(&base, &record.id)?;
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
    let live = match cistella::attestation::query_incarnation(&record.container_name) {
        Ok(observed) if &observed == bound => LiveIncarnation::Current,
        Ok(_) => LiveIncarnation::Stale,
        Err(_) => LiveIncarnation::Unknown,
    };
    let admittability = cistella::describe::exec_admittability(&record.container_name)?;
    Ok(decide_enter(
        attestation.as_ref(),
        &record.container_name,
        &live,
        &admittability,
    ))
}
