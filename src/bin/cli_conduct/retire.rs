//! Session retirement and guest re-hosting.
//!
//! Split from `cli_conduct.rs` at the file-size limit.

use cistella::framework::conduct::{
    host_isolator, release_client, report_release,
};
use cistella::isolators::client::WireClient;

/// Retires a proven-dead pre-exec client and hosts a
/// replacement under the still-held creation-window guard. Close
/// (unconditional join/unlink) frees the rendezvous path, then a
/// fresh host replays the same key. The first-death context
/// reports to stderr at retire time; the second failure dominates
/// downstream. Re-host failure converges directly by name with the
/// lock-held half — the full converge would re-acquire the guard
/// and deadlock — and a residue-dominant report.
///
/// Returns the fresh client, or the terminal error after
/// converging (the caller drops the guard and reports).
pub(crate) fn retire_and_rehost(
    client: WireClient,
    exe_dir: &std::path::Path,
    rendezvous_dir: &std::path::Path,
    container_name: &str,
    session_id: &str,
    first_error: &cistella::error::CistellaError,
    isolator: &cistella::profile::IsolatorResolution,
) -> Result<WireClient, cistella::error::CistellaError> {
    eprintln!(
        "error: pre-exec guest death ({first_error}): retiring client and re-hosting"
    );
    report_release(release_client(client, rendezvous_dir));
    match host_isolator(exe_dir, rendezvous_dir, isolator) {
        Ok(fresh) => Ok(fresh),
        Err(rehost_error) => {
            let teardown_result =
                cistella::runtime::teardown_inner(container_name, session_id);
            let residue_ok = cistella::runtime::residue_gone(container_name);
            Err(cistella::isolators::client::rehost_failure_verdict(
                teardown_result,
                residue_ok,
                rehost_error,
            ))
        },
    }
}
