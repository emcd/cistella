//! Declarative profile file with image, command, mounts, and labels.
//!
//! Cistella knows only profiles: the image tag or digest, the optional
//! harness argv array, the allowlist mount triples, the credential-surface
//! slot, environment exports, and optional generic labels. The harness
//! itself is argv after `--` chosen by the caller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{CistellaError, Result};
use crate::mount::{MountTriple, canonicalize_container_target, validate_mounts};
use crate::session::validate_generic_label;

/// Credential surface: `none` or a per-identity sign-only socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSurface {
    /// No agent is mounted.
    None,
    /// Per-identity socket mounted RO at the same path inside.
    Agent {
        /// Host path of the per-identity `AF_UNIX` socket.
        ssh_agent: String,
    },
}

impl<'de> Deserialize<'de> for CredentialSurface {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;
        struct CsVisitor;
        impl<'de> Visitor<'de> for CsVisitor {
            type Value = CredentialSurface;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("`\"none\"` or `{ ssh_agent = \"/path\" }`")
            }
            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                if v == "none" {
                    Ok(CredentialSurface::None)
                } else {
                    Err(de::Error::custom(
                        "credential_surface string must be \"none\"",
                    ))
                }
            }
            fn visit_string<E>(self, v: String) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(&v)
            }
            fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut ssh_agent: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "ssh_agent" {
                        ssh_agent = Some(map.next_value()?);
                    } else {
                        return Err(de::Error::unknown_field(&key, &["ssh_agent"]));
                    }
                }
                if let Some(s) = ssh_agent {
                    if s.is_empty() || !s.starts_with('/') {
                        return Err(de::Error::custom("ssh_agent must be an absolute path"));
                    }
                    Ok(CredentialSurface::Agent { ssh_agent: s })
                } else {
                    Err(de::Error::custom("missing ssh_agent"))
                }
            }
        }
        deserializer.deserialize_any(CsVisitor)
    }
}

/// Declarative profile loaded from TOML (e.g. `data/profiles/<name>.toml`).
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    /// Image tag or digest, e.g. `localhost/cistella-opencode:example`.
    pub image: String,
    /// Allowlist mount triples (required field, may be an empty list).
    pub mounts: Vec<MountTriple>,
    /// Harness argv array (TOML array, never a shell string).
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Env exports inside the container.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Credential surface slot; `none` mounts nothing, `ssh_agent` mounts per-identity socket RO.
    pub credential_surface: CredentialSurface,
    /// Single distinguished writable session-home root.
    #[serde(default = "default_container_home")]
    pub container_home: String,
    /// Generic labels (CLI `--label` and this table share one rule:
    /// `cistella.` prefix refused, only the driver emits `cistella.*`).
    #[serde(default)]
    pub labels: HashMap<String, String>,
}

fn default_container_home() -> String {
    "/home/cistella".to_string()
}

/// Resolves a profile reference to a file path.
///
/// A reference containing `/` or ending in `.toml` is a file path;
/// otherwise it is a profile name resolved under `data/profiles/` relative
/// to the current directory, falling back to the crate manifest dir.
fn resolve_profile_path(reference: &str) -> Result<PathBuf> {
    if reference.contains('/') || reference.ends_with(".toml") {
        return Ok(PathBuf::from(reference));
    }
    let file = format!("data/profiles/{reference}.toml");
    let cwd_path = PathBuf::from(&file);
    if cwd_path.exists() {
        return Ok(cwd_path);
    }
    let manifest_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("data/profiles/{reference}.toml"));
    if manifest_path.exists() {
        return Ok(manifest_path);
    }
    Err(CistellaError::Profile(format!(
        "profile {reference} not found as {file} (cwd) or {} (manifest)",
        manifest_path.display()
    )))
}

impl Profile {
    /// Loads a profile by name (`data/profiles/<name>.toml`) or file path.
    ///
    /// Returns the profile, the sha256 hex digest of its TOML text, and the
    /// registry name (the reference itself for names, the file stem for
    /// paths — paths never enter labels).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` if the profile cannot be found,
    /// read, parsed, or validated.
    pub fn resolve(reference: &str) -> Result<(Self, String, String)> {
        let path = resolve_profile_path(reference)?;
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CistellaError::Profile(format!("read {}: {e}", path.display())))?;
        let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let name = if reference.contains('/') || reference.ends_with(".toml") {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| reference.to_string())
        } else {
            reference.to_string()
        };
        Ok((Self::from_toml(&raw)?, digest, name))
    }

    /// Loads a profile from a TOML file.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` if the file cannot be read or
    /// parsed, or validation fails.
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| CistellaError::Profile(format!("read {}: {e}", path.display())))?;
        Self::from_toml(&raw)
    }

    /// Returns the sha256 hex digest of TOML text.
    #[must_use]
    pub fn digest_of(text: &str) -> String {
        format!("{:x}", Sha256::digest(text.as_bytes()))
    }

    /// Parses TOML text into a validated profile.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on parse or validation failure.
    pub fn from_toml(text: &str) -> Result<Self> {
        let mut profile: Self =
            toml::from_str(text).map_err(|e| CistellaError::Profile(format!("parse: {e}")))?;
        if profile.image.trim().is_empty() {
            return Err(CistellaError::Profile("image is required".to_string()));
        }
        for ch in ["\n", "\r", "\0"] {
            if profile.image.contains(ch) {
                return Err(CistellaError::Profile(
                    "image must not contain control characters".to_string(),
                ));
            }
        }
        if let Some(command) = &profile.command {
            if command.is_empty() {
                return Err(CistellaError::Profile(
                    "command must be a non-empty argv array".to_string(),
                ));
            }
            for arg in command {
                if arg.contains('\n') || arg.contains('\r') || arg.contains('\0') {
                    return Err(CistellaError::Profile(
                        "command argv must not contain control characters".to_string(),
                    ));
                }
            }
        }
        for (key, value) in &profile.labels {
            validate_generic_label(key, value).map_err(CistellaError::Profile)?;
        }
        // Expand a leading `~` in host sources so shipped profiles stay
        // portable across seats (`~/.config/opencode`, never bare `~`).
        for triple in &mut profile.mounts {
            if triple.host_source == "~" || triple.host_source.starts_with("~/") {
                let home = std::env::var("HOME").map_err(|_| {
                    CistellaError::Profile("HOME not set for ~ expansion".to_string())
                })?;
                triple.host_source = format!("{home}{}", &triple.host_source[1..]);
            }
        }
        // credential_surface is required and validated by Deserialize; `None` is explicitly allowed.
        if profile.container_home.trim().is_empty() {
            profile.container_home = default_container_home();
        }
        if !profile.container_home.starts_with('/') {
            return Err(CistellaError::Profile(
                "container_home must be absolute".to_string(),
            ));
        }
        let canon_home = canonicalize_container_target(&profile.container_home);
        // container_home is not a triple but must not be a sensitive root or traversal to one.
        if canon_home == "/" {
            return Err(CistellaError::Profile(
                "container_home must not be /".to_string(),
            ));
        }
        for root in ["/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64"] {
            if canon_home == root || canon_home.starts_with(&format!("{root}/")) {
                return Err(CistellaError::Profile(format!(
                    "container_home at or above sensitive root {root}: {} (canonical {canon_home})",
                    profile.container_home
                )));
            }
        }
        // Update to canonical form for rendering (prevents Tmpfs= injection via traversal)
        profile.container_home = canon_home;
        // Quadlet injection guard: no newlines/control in container_home/env keys/values, and strict charset
        for ch in ["\n", "\r", "\0"] {
            if profile.container_home.contains(ch) {
                return Err(CistellaError::Profile(
                    "container_home must not contain control characters".to_string(),
                ));
            }
        }
        for (k, v) in &profile.env {
            if k.contains('\n') || k.contains('\r') || v.contains('\n') || v.contains('\r') {
                return Err(CistellaError::Profile(format!(
                    "env key/value must not contain newlines: {k}"
                )));
            }
            if k.is_empty()
                || !k
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
                || !k
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(CistellaError::Profile(format!(
                    "env key must match [A-Z_][A-Z0-9_]*: {k}"
                )));
            }
        }
        // HOME is derived from container_home, not freely overridden via env.
        if profile.env.contains_key("HOME") {
            return Err(CistellaError::Profile(
                "HOME must not be set in env; derived from container_home".to_string(),
            ));
        }
        validate_mounts(&profile.mounts, &profile.container_home)?;
        Ok(profile)
    }

    /// Returns the derived HOME value.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.container_home
    }
}

/// Re-export for doc links.
pub use crate::mount::MountTriple as ProfileMountTriple;
