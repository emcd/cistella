//! Session identity: minted ids, driver labels, and harness argv.
//!
//! `conduct` mints the id (invocation identity, not a config digest) so two
//! concurrent invocations on the same directory never collide. Only the
//! driver emits `cistella.*` labels; generic `--label` keys share one
//! validation rule and are refused the reserved prefix.

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{CistellaError, Result};

/// Label keys emitted only by the driver.
pub const LABEL_ID: &str = "cistella.id";
pub const LABEL_DIRECTORY: &str = "cistella.directory";
pub const LABEL_PROFILE: &str = "cistella.profile";
pub const LABEL_PROFILE_DIGEST: &str = "cistella.profile-digest";
pub const LABEL_IDENTITY: &str = "cistella.identity";
pub const LABEL_COMMAND: &str = "cistella.command";
pub const LABEL_IMAGE: &str = "cistella.image";

/// Reserved prefix: only the driver emits `cistella.*` labels.
pub const RESERVED_PREFIX: &str = "cistella.";

/// Fixed length of a minted session id (9 time chars + 8 entropy chars).
pub const MINTED_ID_LEN: usize = 17;

/// Lowercase Crockford base32 alphabet (`[a-z0-9]`, no `i`/`l`/`o`/`u`).
const CROCKFORD_LOWER: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Mints a time-sortable session id: ms timestamp plus 40 random bits in
/// lowercase Crockford base32, fixed length, `[a-z0-9]`, >= 40 bits entropy.
#[must_use]
pub fn mint_session_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut entropy = [0u8; 5];
    if !read_urandom(&mut entropy) {
        // Fallback when `/dev/urandom` is unavailable: xorshift over
        // time, pid, and a counter (still unique per invocation).
        let mut x = ms
            ^ ((std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15))
            ^ ((entropy.as_ptr() as u64) << 17);
        for b in &mut entropy {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = (x & 0xFF) as u8;
        }
    }
    let mut out = String::with_capacity(MINTED_ID_LEN);
    for shift in (0..9).rev() {
        out.push(CROCKFORD_LOWER[((ms >> (shift * 5)) & 31) as usize] as char);
    }
    let mut rand: u64 = 0;
    for b in entropy {
        rand = (rand << 8) | b as u64;
    }
    for shift in (0..8).rev() {
        out.push(CROCKFORD_LOWER[((rand >> (shift * 5)) & 31) as usize] as char);
    }
    out
}

fn read_urandom(buf: &mut [u8]) -> bool {
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .is_ok()
}

/// Serializes harness argv as a JSON array string for `cistella.command`.
///
/// JSON is lossless (spaces, quotes, `=` survive); shell-escaped strings
/// would not round-trip.
#[must_use]
pub fn command_label(argv: &[String]) -> String {
    serde_json::to_string(argv).unwrap_or_else(|_| "[]".to_string())
}

/// Parses a `cistella.command` JSON array string back into argv.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the label is not a JSON string array.
pub fn parse_command_label(label: &str) -> Result<Vec<String>> {
    serde_json::from_str(label)
        .map_err(|e| CistellaError::Runtime(format!("cistella.command label: {e}")))
}

/// Validates one generic label (`--label k=v` or profile `labels` table).
///
/// Keys and values share one rule: keys match `[A-Za-z0-9._-]+`, values
/// reject Quadlet-invalid `=`/`\n`/`\0`, and keys beginning with
/// `cistella.` are refused (only the driver emits `cistella.*`, last).
///
/// # Errors
///
/// Returns a message describing the violation.
pub fn validate_generic_label(key: &str, value: &str) -> std::result::Result<(), String> {
    if key.is_empty() || key.len() > 128 {
        return Err(format!("label key must be 1-128 chars: {key}"));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        || key.starts_with('-')
    {
        return Err(format!("label key must match [A-Za-z0-9._-]+: {key}"));
    }
    if key.starts_with(RESERVED_PREFIX) {
        return Err(format!(
            "label key reserves prefix {RESERVED_PREFIX}: {key}"
        ));
    }
    if key.contains('\n') || key.contains('\r') || key.contains('\0') {
        return Err(format!(
            "label key must not contain control characters: {key}"
        ));
    }
    if value.contains('=') || value.contains('\n') || value.contains('\r') || value.contains('\0') {
        return Err(format!(
            "label value must not contain '=' or control: {key}"
        ));
    }
    Ok(())
}

/// Splits a CLI `--label k=v` argument on the first `=` and validates it.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the argument has no `=` or fails
/// generic label validation.
pub fn parse_cli_label(arg: &str) -> Result<(String, String)> {
    let Some((key, value)) = arg.split_once('=') else {
        return Err(CistellaError::Runtime(format!(
            "--label must be k=v: {arg}"
        )));
    };
    validate_generic_label(key, value).map_err(CistellaError::Runtime)?;
    Ok((key.to_string(), value.to_string()))
}

/// Session identity minted by `conduct`.
#[derive(Debug, Clone)]
pub struct Session {
    /// Minted id (`[a-z0-9]`, fixed length, time-sortable).
    pub id: String,
    /// Canonical host directory mounted at `/work`.
    pub directory: String,
    /// Profile name.
    pub profile: String,
    /// Sha256 hex digest of the profile TOML.
    pub profile_digest: String,
    /// Identity label (not a credential selector).
    pub identity: String,
    /// Harness argv actually executed.
    pub command: Vec<String>,
    /// Resolved image digest.
    pub image: String,
    /// Canonical container home (for `Tmpfs=`).
    pub container_home: String,
}

impl Session {
    /// Returns the container name (`cistella-<id>`, no harness).
    #[must_use]
    pub fn container_name(&self) -> String {
        format!("cistella-{}", self.id)
    }

    /// Returns the Quadlet unit name (`cistella-<id>.container`).
    #[must_use]
    pub fn quadlet_unit_name(&self) -> String {
        format!("cistella-{}.container", self.id)
    }
}

/// Rejects Quadlet directive injection via control characters.
pub(crate) fn ensure_no_injection(val: &str, field: &str) -> Result<()> {
    if val.contains('\n') || val.contains('\r') || val.contains('\0') {
        return Err(CistellaError::Runtime(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

/// Validates a minted id (`[a-z0-9]`, fixed length).
pub(crate) fn ensure_minted_id(val: &str) -> Result<()> {
    if val.len() != MINTED_ID_LEN
        || !val
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(CistellaError::Runtime(format!(
            "session id must be {MINTED_ID_LEN} lowercase [a-z0-9] chars"
        )));
    }
    Ok(())
}

/// Validates a session text field (`profile`, `identity`).
pub(crate) fn ensure_session_field(val: &str, field: &str) -> Result<()> {
    ensure_no_injection(val, field)?;
    if val.is_empty() || val.len() > 64 {
        return Err(CistellaError::Runtime(format!(
            "{field} must be 1-64 chars"
        )));
    }
    if !val
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(CistellaError::Runtime(format!(
            "{field} must match [A-Za-z0-9._-]+: {val}"
        )));
    }
    // Also reject leading dash which could be parsed as flag in systemd Label=
    if val.starts_with('-') {
        return Err(CistellaError::Runtime(format!(
            "{field} must not start with '-': {val}"
        )));
    }
    Ok(())
}
