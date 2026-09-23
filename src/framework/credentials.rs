//! Credential seam: strict handle variants, never values (task 2.3).
//!
//! Credential contributions use handle variants only. Each admitted
//! kind declares a bounded locator grammar with a namespace,
//! framework-owned resolution locality, and unknown-field rejection.
//! Handles are framework-issued opaque identifiers or
//! registry-resolved against a pre-existing framework-owned
//! registry — never user-chosen strings, so no secret can hide in an
//! arbitrary permitted string. Locator sensitivity is explicit per
//! kind with diagnostic redaction rules; diagnostics name the handle
//! kind and locator class, never content. The schema provides no
//! designated secret-value channel: value-shaped fields refuse.
//! Raw extension protocol stderr stays discarded (never stored,
//! never forwarded, never rendered).
//!
//! Each admitted handle kind requires separate tier-2 review of its
//! resolution path. Two kinds ship in 0.2.0:
//!
//! - `opaque-reference`: framework-issued opaque identifier minted at
//!   `create`/`execute_launch` time (mirrors [`UnitHandle`]/
//!   [`ExecutionHandle`]); resolution locality is the issuing
//!   framework instance.
//! - `seat-socket`: bounded absolute-path locator into the
//!   framework-owned seat runtime directory (mirrors the profile
//!   `credential_surface` socket mount); resolution locality is the
//!   seat dir, and paths escaping it refuse.
//!
//! Capability advertisement for the seam is proven by the
//! deterministic fake (forbidden shapes refused, admitted handles
//! accepted, oversized strings refused), never by real credential
//! transport. Real transport wiring rides the dogfood gate (4.1).
//!
//! Review record: each admitted kind's resolution path (where
//! `opaque-reference` ids resolve to backing resources, where
//! `seat-socket` paths translate to mounts) lands separately with
//! its own tier-2 review, never folded into one packet.
//!
//! Wire direction is read-only: [`CredentialHandle`] is
//! deserialize-only by construction, so handles never round-trip
//! back to a guest or to disk even if a future path tries.

use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

use crate::error::{CistellaError, Result};

/// Maximum locator string length (oversize refuses before use).
pub const MAX_LOCATOR_LEN: usize = 1024;

/// Seat runtime directory anchor for `seat-socket` locators.
///
/// Resolution locality: locators must stay under this prefix after
/// lexical normalization; anything else refuses.
pub const SEAT_RUNTIME_DIR: &str = "/run/cistella/seats";

/// Socket filename grammar inside the seat directory.
const SOCKET_NAME_PATTERN: &str = r"^[A-Za-z0-9_.-]{1,128}$";

/// Opaque identifier grammar (framework-minted shape).
const OPAQUE_ID_PATTERN: &str = r"^[a-z0-9]{1,128}$";

/// Opaque identifier grammar, compiled once per process.
static OPAQUE_ID_GRAMMAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(OPAQUE_ID_PATTERN).expect("opaque pattern compiles"));

/// Socket filename grammar, compiled once per process.
static SOCKET_NAME_GRAMMAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SOCKET_NAME_PATTERN).expect("socket pattern compiles"));

/// Credential handle variants: the only admissible shapes.
///
/// Unknown variants refuse (`deny_unknown_fields` plus an
/// explicitly-tagged enum): a guest that invents a credential kind
/// fails the transaction.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "kebab-case")]
pub enum CredentialHandle {
    /// Framework-issued opaque reference.
    OpaqueReference {
        /// Opaque identifier (minted shape, never user-chosen).
        id: String,
    },
    /// Bounded socket path inside the framework-owned seat dir.
    SeatSocket {
        /// Absolute socket path under [`SEAT_RUNTIME_DIR`].
        path: String,
    },
}

/// Validated credential contribution: kind plus redacted locator class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedCredential {
    /// Handle kind name for diagnostics.
    pub kind: &'static str,
    /// Locator class (never content): `opaque-id` or `seat-socket`.
    pub locator_class: &'static str,
}

impl CredentialHandle {
    /// Validates one handle: grammar, bounds, locality.
    ///
    /// Diagnostics name the kind and locator class, never content:
    /// paths and identifiers are secret-adjacent and never render.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on grammar, bound, or
    /// locality violations.
    pub fn admit(&self) -> Result<AdmittedCredential> {
        match self {
            Self::OpaqueReference { id } => {
                if id.len() > MAX_LOCATOR_LEN {
                    return Err(CistellaError::Contract(
                        "credential handle oversize: opaque-reference".to_string(),
                    ));
                }
                if !OPAQUE_ID_GRAMMAR.is_match(id) {
                    return Err(CistellaError::Contract(
                        "credential handle refused: opaque-reference grammar".to_string(),
                    ));
                }
                Ok(AdmittedCredential {
                    kind: "opaque-reference",
                    locator_class: "opaque-id",
                })
            }
            Self::SeatSocket { path } => {
                if path.len() > MAX_LOCATOR_LEN {
                    return Err(CistellaError::Contract(
                        "credential handle oversize: seat-socket".to_string(),
                    ));
                }
                if !path.starts_with('/') {
                    return Err(CistellaError::Contract(
                        "credential handle refused: seat-socket must be absolute".to_string(),
                    ));
                }
                if !path.starts_with(&format!("{SEAT_RUNTIME_DIR}/")) {
                    return Err(CistellaError::Contract(
                        "credential handle refused: seat-socket outside seat runtime dir"
                            .to_string(),
                    ));
                }
                let name = path.rsplit('/').next().unwrap_or_default();
                if !SOCKET_NAME_GRAMMAR.is_match(name) {
                    return Err(CistellaError::Contract(
                        "credential handle refused: seat-socket name grammar".to_string(),
                    ));
                }
                if path.contains("/../") || path.ends_with("/..") {
                    return Err(CistellaError::Contract(
                        "credential handle refused: seat-socket traversal".to_string(),
                    ));
                }
                Ok(AdmittedCredential {
                    kind: "seat-socket",
                    locator_class: "seat-socket",
                })
            }
        }
    }
}

/// Validates a batch of credential handles from one prepare response.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on the first refusing handle;
/// atomic with the transaction (no partial admission).
pub fn admit_all(handles: &[CredentialHandle]) -> Result<Vec<AdmittedCredential>> {
    handles.iter().map(CredentialHandle::admit).collect()
}

/// Parses credential handles from a wire value with unknown-field and
/// unknown-variant refusal.
///
/// The JSON schema has no secret-value channel: any object that is
/// not exactly one of the admitted variants refuses here, before
/// grammar checks.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on shape violations.
pub fn parse_wire(value: &serde_json::Value) -> Result<Vec<CredentialHandle>> {
    serde_json::from_value(value.clone())
        .map_err(|e| CistellaError::Contract(format!("bad credential handle: {e}")))
}
