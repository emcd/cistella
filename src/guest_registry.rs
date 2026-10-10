//! Seat-local third-party guest registry (registration replacing
//! closed admission).
//!
//! One explicit file per seat (`registry.toml` under the XDG config
//! base, `format-version = 1` required) maps exact guest names to
//! verified sources — fetch URL plus sha256, or absolute filesystem
//! path — plus the exact hello role string and static argv. Two
//! tables, `[[isolators]]` and `[[extensions]]`: two genera, never
//! unified (companion pairs across tables are legal by design).
//!
//! Trust posture: the file opens `O_NOFOLLOW` as a regular file, with
//! uid/mode checked by fstat on that same FD and the bounded content
//! read and parsed from that same FD (opening to stat and reopening
//! to read would admit a swap between check and parse — the same gap
//! the binary-source anchor closes). Absent means empty (exact 0.2.0
//! behavior); malformed refuses every conduct pre-create loudly
//! (eager fail-closed: a silently-ignored registry could plain a
//! meant-hooked session). Unknown keys refuse at parse (no inert
//! fields — nothing unverified is assumable). The registry file
//! itself is local only: no registry URL or bootstrap fetch exists.
//!
//! Environment is taken as an explicit snapshot ([`HostEnv`]) so unit
//! tests pin behavior without touching process env; [`load`] reads
//! the live seat environment.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{CistellaError, Result};
use crate::guest_anchor::begin_anchored;

/// Required registry `format-version` (absent or otherwise refuses).
pub const REGISTRY_FORMAT_VERSION: u32 = 1;

/// Read ceiling for the registry file itself: TOML text, never a
/// binary. Sized far above any plausible registry; the security
/// properties (nofollow, regular-file, fstat bind) do not depend on
/// it.
const MAX_REGISTRY_FILE_BYTES: u64 = 1024 * 1024;

/// Reserved shipped name in `[[isolators]]`: entries shadowing it
/// refuse at parse (a shadowing entry is a typo or a confusion
/// attack, never legitimate).
pub const SHIPPED_ISOLATOR_NAME: &str = "podman";

/// Reserved shipped name in `[[extensions]]`: entries shadowing it
/// refuse at parse. Note the deliberate asymmetry: a `landlock`
/// role string in `[[isolators]]` stays legal for the planned future
/// sibling (guarded at runtime by the slot check plus the string
/// compare), while no podman extension exists or is planned.
pub const SHIPPED_EXTENSION_NAME: &str = "landlock";

/// Seat environment snapshot for registry location. Explicit so
/// tests pin XDG/HOME shapes without mutating process env.
#[derive(Debug, Clone)]
pub struct HostEnv {
    /// `XDG_CONFIG_HOME` value (`None` when unset).
    pub xdg_config_home: Option<String>,
    /// `XDG_CACHE_HOME` value (`None` when unset).
    pub xdg_cache_home: Option<String>,
    /// `HOME` value (`None` when unset).
    pub home: Option<String>,
}

impl HostEnv {
    /// Reads the live seat environment.
    #[must_use]
    pub fn live() -> Self {
        Self {
            xdg_config_home: std::env::var("XDG_CONFIG_HOME").ok(),
            xdg_cache_home: std::env::var("XDG_CACHE_HOME").ok(),
            home: std::env::var("HOME").ok(),
        }
    }

    /// Fabricates an environment snapshot (tests).
    #[must_use]
    pub fn new(
        xdg_config_home: Option<&str>,
        xdg_cache_home: Option<&str>,
        home: Option<&str>,
    ) -> Self {
        Self {
            xdg_config_home: xdg_config_home.map(str::to_string),
            xdg_cache_home: xdg_cache_home.map(str::to_string),
            home: home.map(str::to_string),
        }
    }
}

/// Resolves the single registry file path from an environment
/// snapshot: `${XDG_CONFIG_HOME}/cistella/registry.toml`, falling
/// back to `$HOME/.config/cistella/registry.toml` when XDG is unset
/// or empty (same fallback as profile resolution — an unset XDG var
/// is normal-seat condition, never an error).
///
/// A nonempty `XDG_CONFIG_HOME` must be absolute (XDG requires
/// absolute paths); a relative value refuses with a typed error — a
/// relative base would resolve the registry against the conduct
/// worktree, letting a repo-local file become an unreviewed registry.
/// A `HOME` that is itself unset or empty refuses with a typed
/// error, since no seat path can be formed.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on relative XDG or missing HOME.
/// Diagnostics name the variable, never values.
pub fn registry_path(env: &HostEnv) -> Result<PathBuf> {
    let base = match env.xdg_config_home.as_deref().filter(|s| !s.is_empty()) {
        Some(dir) => {
            let path = PathBuf::from(dir);
            if !path.is_absolute() {
                return Err(CistellaError::Profile(
                    "XDG_CONFIG_HOME must be absolute for registry resolution".to_string(),
                ));
            }
            path
        }
        None => {
            let home = env
                .home
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    CistellaError::Profile("HOME not set for registry resolution".to_string())
                })?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("cistella/registry.toml"))
}

/// Raw registry file shape (TOML): version plus the two genera tables.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RegistryFile {
    /// Schema version (must equal [`REGISTRY_FORMAT_VERSION`];
    /// `Option` so absence refuses with the version diagnostic,
    /// not a missing-field error).
    format_version: Option<u32>,
    /// Third-party isolator entries (default none).
    #[serde(default)]
    isolators: Vec<GuestEntry>,
    /// Third-party extension entries (default none).
    #[serde(default)]
    extensions: Vec<GuestEntry>,
}

/// Raw registry entry shape (TOML): exact name, role, protocol, hash,
/// exactly one source, static argv.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct GuestEntry {
    /// Exact registry name (nonempty, unique per table).
    name: String,
    /// Exact hello role string the guest must advertise.
    role: String,
    /// Wire protocol major the guest speaks.
    protocol: u32,
    /// Expected sha256 over the executed bytes (64 lowercase hex).
    sha256: String,
    /// Fetch source URL (exactly one of `url`/`path`).
    url: Option<String>,
    /// Absolute filesystem source path (exactly one of `url`/`path`).
    path: Option<String>,
    /// Static guest argv, passed verbatim (default none).
    #[serde(default)]
    args: Vec<String>,
}

/// Validated entry source: fetch URL or absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestSource {
    /// Fetch URL (http/https) plus expected sha256.
    Fetch {
        /// Source URL (transport untrusted; hash is the anchor).
        url: String,
        /// Expected sha256 over the fetched bytes.
        sha256: [u8; 32],
    },
    /// Absolute filesystem path plus expected sha256.
    Path {
        /// Absolute source path (canonicalized at resolve).
        path: PathBuf,
        /// Expected sha256 over the resolved bytes.
        sha256: [u8; 32],
    },
}

/// Validated registry entry: parse- and schema-checked, ready to
/// resolve (fetch/download or path open plus digest verify).
#[derive(Debug, Clone)]
pub struct RegisteredGuest {
    /// Exact registry name.
    pub name: String,
    /// Exact hello role string the guest must advertise.
    pub role: String,
    /// Wire protocol major the guest speaks.
    pub protocol: u32,
    /// Verified source (fetch URL or absolute path plus hash).
    pub source: GuestSource,
    /// Static guest argv, passed verbatim to the spawn.
    pub args: Vec<String>,
}

/// Which genera table an entry lives in (the table determines the
/// kind: `[[isolators]]` resolves only from `[isolator]`,
/// `[[extensions]]` only from `[[extensions]]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestTable {
    /// Isolator genera (`[[isolators]]`).
    Isolators,
    /// Extension genera (`[[extensions]]`).
    Extensions,
}

/// Validated seat registry: the two genera tables (either may be
/// empty; an absent file loads as empty, exact 0.2.0 behavior).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    /// Validated `[[isolators]]` entries.
    pub isolators: Vec<RegisteredGuest>,
    /// Validated `[[extensions]]` entries.
    pub extensions: Vec<RegisteredGuest>,
}

impl Registry {
    /// Loads the seat registry from the live environment: resolves
    /// the single file path, then loads from it (absent means empty;
    /// malformed refuses — eager fail-closed for every conduct).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on path, anchor, parse, or
    /// schema refusal.
    pub fn load() -> Result<Self> {
        Self::load_from_env(&HostEnv::live())
    }

    /// Loads the seat registry from an environment snapshot (tests
    /// pin XDG/HOME shapes here without process env).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on path, anchor, parse, or
    /// schema refusal.
    pub fn load_from_env(env: &HostEnv) -> Result<Self> {
        Self::load_from_path(&registry_path(env)?)
    }

    /// Loads the registry from one explicit file path: absent means
    /// empty; present opens nofollow with uid/mode fstat plus bounded
    /// read/parse on the same FD (no check-then-reopen gap).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on anchor, parse, or schema
    /// refusal, and `CistellaError::Io` on unreadable-file failure.
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let Some(open) = begin_anchored(path, MAX_REGISTRY_FILE_BYTES)? else {
            return Ok(Self::default());
        };
        let anchored = open.finish()?;
        let text = std::str::from_utf8(&anchored.bytes).map_err(|_| {
            CistellaError::Profile(format!("registry {} is not valid UTF-8", path.display()))
        })?;
        Self::parse(text)
    }

    /// Parses and validates registry TOML text (pure: no I/O, no
    /// env — the unit-test entry point).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on TOML, version, or entry
    /// refusal. Diagnostics name declarations, never values.
    pub fn parse(text: &str) -> Result<Self> {
        let file: RegistryFile = toml::from_str(text)
            .map_err(|e| CistellaError::Profile(format!("registry parse: {e}")))?;
        if file.format_version != Some(REGISTRY_FORMAT_VERSION) {
            return Err(CistellaError::Profile(format!(
                "registry format-version must be {REGISTRY_FORMAT_VERSION}"
            )));
        }
        let isolators = validate_table(file.isolators, GuestTable::Isolators)?;
        let extensions = validate_table(file.extensions, GuestTable::Extensions)?;
        Ok(Self {
            isolators,
            extensions,
        })
    }

    /// Finds an entry by exact name in one genera table.
    #[must_use]
    pub fn find(&self, table: GuestTable, name: &str) -> Option<&RegisteredGuest> {
        match table {
            GuestTable::Isolators => self.isolators.iter().find(|e| e.name == name),
            GuestTable::Extensions => self.extensions.iter().find(|e| e.name == name),
        }
    }

    /// Finds an entry by exact name in either table (slot-error
    /// diagnostics: a name registered in the other table refuses as
    /// a wrong-slot entry, never as unknown).
    #[must_use]
    pub fn find_any(&self, name: &str) -> Option<(GuestTable, &RegisteredGuest)> {
        if let Some(entry) = self.find(GuestTable::Isolators, name) {
            return Some((GuestTable::Isolators, entry));
        }
        if let Some(entry) = self.find(GuestTable::Extensions, name) {
            return Some((GuestTable::Extensions, entry));
        }
        None
    }

    /// True when both tables are empty (absent-file behavior).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.isolators.is_empty() && self.extensions.is_empty()
    }
}

/// Validates one genera table: nonempty names, per-table duplicate
/// refusal, reserved-name shadowing refusal, per-entry checks.
fn validate_table(entries: Vec<GuestEntry>, table: GuestTable) -> Result<Vec<RegisteredGuest>> {
    let reserved = match table {
        GuestTable::Isolators => SHIPPED_ISOLATOR_NAME,
        GuestTable::Extensions => SHIPPED_EXTENSION_NAME,
    };
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.name.is_empty() {
            return Err(CistellaError::Profile(
                "registry entry name must not be empty".to_string(),
            ));
        }
        reject_spans(&entry.name, "registry entry name")?;
        if !seen.insert(entry.name.clone()) {
            return Err(CistellaError::Profile(format!(
                "duplicate registry entry: {}",
                entry.name
            )));
        }
        if entry.name == reserved {
            return Err(CistellaError::Profile(format!(
                "registry entry shadows shipped name: {}",
                entry.name
            )));
        }
        out.push(validate_entry(entry, table)?);
    }
    Ok(out)
}

/// Validates one entry: role shape plus the deliberate role-string
/// asymmetry (`podman` in `[[extensions]]` refuses at parse — no
/// podman extension exists or is planned; `landlock` in
/// `[[isolators]]` stays legal for the future sibling), sha256
/// shape, exactly-one-source, static-argv span refusal.
fn validate_entry(entry: GuestEntry, table: GuestTable) -> Result<RegisteredGuest> {
    if entry.role.is_empty() {
        return Err(CistellaError::Profile(format!(
            "registry entry {} role must not be empty",
            entry.name
        )));
    }
    reject_spans(&entry.role, "registry entry role")?;
    if table == GuestTable::Extensions && entry.role == SHIPPED_ISOLATOR_NAME {
        return Err(CistellaError::Profile(format!(
            "registry entry {} records reserved isolator role string",
            entry.name
        )));
    }
    let sha256 = parse_sha256_hex(&entry.sha256).map_err(|_| {
        CistellaError::Profile(format!("registry entry {} sha256 malformed", entry.name))
    })?;
    for arg in &entry.args {
        reject_spans(arg, "registry entry arg")?;
    }
    let source = match (entry.url, entry.path) {
        (Some(url), None) => {
            reject_spans(&url, "registry entry url")?;
            if !url.starts_with("https://") && !url.starts_with("http://") {
                return Err(CistellaError::Profile(format!(
                    "registry entry {} url must be http(s)",
                    entry.name
                )));
            }
            GuestSource::Fetch { url, sha256 }
        }
        (None, Some(path)) => {
            reject_spans(&path, "registry entry path")?;
            let path = PathBuf::from(&path);
            if !path.is_absolute() {
                return Err(CistellaError::Profile(format!(
                    "registry entry {} path must be absolute",
                    entry.name
                )));
            }
            GuestSource::Path { path, sha256 }
        }
        (Some(_), Some(_)) => {
            return Err(CistellaError::Profile(format!(
                "registry entry {} must declare exactly one source",
                entry.name
            )));
        }
        (None, None) => {
            return Err(CistellaError::Profile(format!(
                "registry entry {} must declare a source",
                entry.name
            )));
        }
    };
    Ok(RegisteredGuest {
        name: entry.name,
        role: entry.role,
        protocol: entry.protocol,
        source,
        args: entry.args,
    })
}

/// Refuses template-interpolation spans anywhere in registry text:
/// no session, environment, or supplement values ever expand into
/// registry fields (the prepare-payload precedent: session-blind
/// values are never legitimate). A span-shaped static value refuses
/// rather than expands.
fn reject_spans(value: &str, what: &str) -> Result<()> {
    if value.contains("{{") {
        return Err(CistellaError::Profile(format!(
            "{what} must not contain template spans"
        )));
    }
    Ok(())
}

/// Parses 64 lowercase hex into raw digest bytes (same shape the
/// merge gate enforces for hook digests).
fn parse_sha256_hex(value: &str) -> std::result::Result<[u8; 32], ()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(());
    }
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk).map_err(|_| ())?;
        out[index] = u8::from_str_radix(text, 16).map_err(|_| ())?;
    }
    Ok(out)
}
