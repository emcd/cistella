//! Declarative profile file with credential-surface and container-home.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::{CistellaError, Result};
use crate::mount::{MountTriple, canonicalize_container_target, validate_mounts};

/// Credential surface: `none` or a per-seat sign-only socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSurface {
    /// No agent is mounted.
    None,
    /// Per-seat socket mounted RO at the same path inside.
    Agent {
        /// Host path of the per-seat `AF_UNIX` socket.
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

/// Declarative profile loaded from TOML (e.g. `coders.toml` profile).
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    /// Harness name, e.g. `opencode`.
    pub harness: String,
    /// Allowlist mount triples.
    #[serde(default)]
    pub mounts: Vec<MountTriple>,
    /// Env exports inside the container.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Credential surface slot; `none` mounts nothing, `ssh_agent` mounts per-seat socket RO.
    pub credential_surface: CredentialSurface,
    /// Single distinguished writable session-home root.
    #[serde(default = "default_container_home")]
    pub container_home: String,
}

fn default_container_home() -> String {
    "/home/cistella".to_string()
}

impl Profile {
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

    /// Parses TOML text into a validated profile.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on parse or validation failure.
    pub fn from_toml(text: &str) -> Result<Self> {
        let mut profile: Self =
            toml::from_str(text).map_err(|e| CistellaError::Profile(format!("parse: {e}")))?;
        if profile.harness.trim().is_empty() {
            return Err(CistellaError::Profile("harness is required".to_string()));
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
        // Harness charset: session-related field
        if !profile
            .harness
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
            || profile.harness.is_empty()
            || profile.harness.len() > 64
            || profile.harness.starts_with('-')
        {
            return Err(CistellaError::Profile(
                "harness must match [A-Za-z0-9._-]{1,64}".to_string(),
            ));
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
            if k.contains('=') || v.contains('=') && v.contains('\n') {
                return Err(CistellaError::Profile(format!(
                    "env must not contain injection: {k}"
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
