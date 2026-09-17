//! Declarative profile file with image, command, mounts, and labels.
//!
//! Cistella knows only profiles: the image tag or digest, the optional
//! harness argv array, the allowlist mount triples, the credential-surface
//! slot, environment exports, and optional generic labels. The harness
//! itself is argv after `--` chosen by the caller.
//!
//! Name resolution order: explicit filesystem path, then supplied
//! configuration-directory tiers (`--configuration-directory`, then
//! `$CISTELLA_CONFIGURATION_DIRECTORY`, each a closed tier), then the XDG
//! user profiles dir, then the baked-in examples. `data/profiles/*.toml`
//! are examples compiled into the binary with `include_str!` (the source
//! dir is the single place to edit them); adding a file there requires
//! registering it in [`BAKED_PROFILES`]. When a named lookup reaches the
//! default tier, [`seed_baked_examples`] copies missing baked examples
//! into the XDG dir (never overwriting user files). There is no
//! cwd-relative lookup and no development-directory detection: a name
//! resolves identically from any working directory.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write as _};
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

/// Declarative profile loaded from TOML (a baked example, an XDG user
/// copy, a configuration-directory file, or an explicit path).
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

/// Baked-in profile examples, compiled from `data/profiles/*.toml`.
///
/// The source dir stays the single place to edit examples; the binary
/// carries them at compile time so names resolve without a source
/// checkout. Register any new file in this table.
const BAKED_PROFILES: &[(&str, &str)] = &[
    (
        "default",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/profiles/default.toml"
        )),
    ),
    (
        "opencode",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/profiles/opencode.toml"
        )),
    ),
];

/// Returns the baked example text for a profile name, if one exists.
fn baked_example(name: &str) -> Option<&'static str> {
    BAKED_PROFILES
        .iter()
        .find_map(|(n, text)| (*n == name).then_some(*text))
}

/// Name-lookup inputs: supplied closed tiers plus the XDG default tier.
///
/// `configuration_directories` holds the `--configuration-directory` flag
/// value first, then `$CISTELLA_CONFIGURATION_DIRECTORY`; each names
/// `<dir>/profiles/<name>.toml` and is closed (a missing name errors, no
/// fallthrough, never seeded). `xdg_profiles_dir` is the default tier
/// (`${XDG_CONFIG_HOME}/cistella/profiles`, `~/.config` fallback).
#[derive(Debug, Clone)]
pub struct ResolutionSource {
    /// Supplied configuration directories in precedence order.
    pub configuration_directories: Vec<PathBuf>,
    /// XDG user profiles directory (seeded from baked examples on reach).
    pub xdg_profiles_dir: PathBuf,
}

impl ResolutionSource {
    /// Builds lookup inputs from explicit parts (tests and callers that
    /// already resolved the environment).
    #[must_use]
    pub fn new(configuration_directories: Vec<PathBuf>, xdg_profiles_dir: PathBuf) -> Self {
        Self {
            configuration_directories,
            xdg_profiles_dir,
        }
    }

    /// Builds lookup inputs from the host environment.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` when neither `XDG_CONFIG_HOME` nor
    /// `HOME` yields a config base.
    pub fn from_host_env(configuration_directory: Option<&str>) -> Result<Self> {
        let mut configuration_directories = Vec::new();
        if let Some(dir) = configuration_directory.filter(|s| !s.is_empty()) {
            configuration_directories.push(PathBuf::from(dir));
        }
        if let Ok(dir) = std::env::var("CISTELLA_CONFIGURATION_DIRECTORY")
            && !dir.is_empty()
        {
            configuration_directories.push(PathBuf::from(dir));
        }
        Ok(Self {
            configuration_directories,
            xdg_profiles_dir: xdg_config_base()?.join("cistella/profiles"),
        })
    }
}

/// Returns the XDG config base: `$XDG_CONFIG_HOME`, else `~/.config`.
fn xdg_config_base() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME")
        .map_err(|_| CistellaError::Profile("HOME not set for XDG config base".to_string()))?;
    Ok(PathBuf::from(home).join(".config"))
}

/// Copies baked examples missing from `dir` (never overwrites user files).
///
/// Directory creation plus per-file `create_new` writes make seeding atomic
/// under races (`AlreadyExists` is tolerated). Written for reuse by a
/// future `cistella init`; `resolve` is its only caller today.
///
/// # Errors
///
/// Returns `CistellaError::Profile` when the directory cannot be created
/// or a missing example cannot be written.
pub fn seed_baked_examples(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| CistellaError::Profile(format!("create {}: {e}", dir.display())))?;
    for (name, text) in BAKED_PROFILES {
        let path = dir.join(format!("{name}.toml"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => file
                .write_all(text.as_bytes())
                .map_err(|e| CistellaError::Profile(format!("seed {}: {e}", path.display())))?,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(CistellaError::Profile(format!(
                    "seed {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// Resolves a profile name against lookup inputs.
///
/// A reference containing `/` or ending in `.toml` is a filesystem path
/// and bypasses every tier (a missing path errors without creating
/// anything). Otherwise it is a name resolved in order: the first supplied
/// configuration directory as a closed tier (the flag shadows the env var;
/// a missing name is a typed error naming the profile, with no fallthrough
/// and no seeding), then the default tier — seed baked examples into the
/// XDG dir when reached, then the XDG copy, then the baked example. A name
/// found nowhere is a typed error naming the profile and every tier
/// searched.
///
/// # Errors
///
/// Returns `CistellaError::Profile` if the profile cannot be found, read,
/// parsed, or validated.
fn resolve_profile_path(
    reference: &str,
    source: &ResolutionSource,
) -> Result<(PathBuf, ProfileText)> {
    if reference.contains('/') || reference.ends_with(".toml") {
        let path = PathBuf::from(reference);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CistellaError::Profile(format!("read {}: {e}", path.display())))?;
        return Ok((path, ProfileText::File(raw)));
    }
    if let Some(dir) = source.configuration_directories.first() {
        let path = dir.join("profiles").join(format!("{reference}.toml"));
        match std::fs::read_to_string(&path) {
            Ok(raw) => return Ok((path, ProfileText::Named(raw))),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(CistellaError::Profile(format!(
                    "profile {reference} not found in configuration directory {} (closed tier, no fallthrough)",
                    dir.display()
                )));
            }
            Err(e) => {
                return Err(CistellaError::Profile(format!(
                    "read {}: {e}",
                    path.display()
                )));
            }
        }
    }
    seed_baked_examples(&source.xdg_profiles_dir)?;
    let xdg_path = source.xdg_profiles_dir.join(format!("{reference}.toml"));
    match std::fs::read_to_string(&xdg_path) {
        Ok(raw) => return Ok((xdg_path, ProfileText::Named(raw))),
        Err(e) if e.kind() != ErrorKind::NotFound => {
            return Err(CistellaError::Profile(format!(
                "read {}: {e}",
                xdg_path.display()
            )));
        }
        Err(_) => {}
    }
    if let Some(baked) = baked_example(reference) {
        return Ok((xdg_path, ProfileText::Named(baked.to_string())));
    }
    let mut searched = vec![
        format!("XDG {}", source.xdg_profiles_dir.display()),
        "baked examples".to_string(),
    ];
    if let Some(dir) = source.configuration_directories.first() {
        searched.insert(0, format!("configuration directory {}", dir.display()));
    }
    Err(CistellaError::Profile(format!(
        "profile {reference} not found (searched {})",
        searched.join(", ")
    )))
}

/// Profile text plus whether it came from a named tier or an explicit path.
enum ProfileText {
    /// Named-tier text: the registry name is the reference itself.
    Named(String),
    /// Explicit-path text: the registry name is the file stem.
    File(String),
}

impl Profile {
    /// Loads a profile by name (configuration directories, then XDG with
    /// seed-if-absent, then baked examples) or file path, using the host
    /// environment for lookup inputs and no supplied tiers.
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
        Self::resolve_in(reference, &ResolutionSource::from_host_env(None)?, None)
    }

    /// Loads a profile by name or file path against explicit lookup inputs
    /// and an optional project name for template expansion.
    ///
    /// Returns the profile, the sha256 hex digest of its TOML text, and the
    /// registry name (the reference itself for names, the file stem for
    /// paths — paths never enter labels).
    ///
    /// `project` is `Some` on the conduct path (explicit flag or directory
    /// default, both validated lazily at expansion) and `None` for
    /// context-free callers: template-free profiles resolve literally,
    /// template-bearing profiles are a typed error. Nothing derives from
    /// the working directory outside conduct.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` if the profile cannot be found,
    /// read, parsed, expanded, or validated.
    pub fn resolve_in(
        reference: &str,
        source: &ResolutionSource,
        project: Option<ProjectName<'_>>,
    ) -> Result<(Self, String, String)> {
        let (path, text) = resolve_profile_path(reference, source)?;
        let (raw, name) = match text {
            ProfileText::Named(raw) => (raw, reference.to_string()),
            ProfileText::File(raw) => {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| reference.to_string());
                (raw, stem)
            }
        };
        let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let mut profile = parse_profile(&raw)?;
        expand_templates(&mut profile, project)?;
        Ok((validate_profile(profile)?, digest, name))
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

    /// Parses TOML text into a validated profile (literal: no template
    /// expansion — use resolution for template-bearing profiles).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Profile` on parse or validation failure.
    pub fn from_toml(text: &str) -> Result<Self> {
        validate_profile(parse_profile(text)?)
    }

    /// Returns the derived HOME value.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.container_home
    }
}
/// Parses TOML text plus leading-`~` expansion, without validation or
/// template expansion.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on parse failure or missing `HOME`
/// for `~` expansion.
fn parse_profile(text: &str) -> Result<Profile> {
    let mut profile: Profile =
        toml::from_str(text).map_err(|e| CistellaError::Profile(format!("parse: {e}")))?;
    // Expand a leading `~` in host sources so shipped profiles stay
    // portable across seats (`~/.config/opencode`, never bare `~`).
    for triple in &mut profile.mounts {
        if triple.host_source == "~" || triple.host_source.starts_with("~/") {
            let home = std::env::var("HOME")
                .map_err(|_| CistellaError::Profile("HOME not set for ~ expansion".to_string()))?;
            triple.host_source = format!("{home}{}", &triple.host_source[1..]);
        }
    }
    Ok(profile)
}

/// Validates a parsed profile: literal text or template-expanded values.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on any validation failure.
fn validate_profile(mut profile: Profile) -> Result<Profile> {
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
    // credential_surface is required and validated by Deserialize; `None` is explicitly allowed.
    profile.container_home = normalize_container_home(&profile.container_home)?;
    let canon_home = profile.container_home.clone();
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

/// Default project name: basename of the canonical session directory.
///
/// Conduct calls this after canonicalization when `--project-name` is
/// absent; agentmux agrees on the same default, so both sides match with
/// no flags.
///
/// # Errors
///
/// Returns `CistellaError::Profile` when the directory has no basename
/// (e.g. filesystem root).
///
/// # Examples
///
/// ```
/// # use cistella::profile::default_project_name;
/// assert_eq!(
///     default_project_name("/home/me/src/CLONES/cistella/qa").unwrap(),
///     "qa"
/// );
/// ```
pub fn default_project_name(directory: &str) -> Result<String> {
    std::path::Path::new(directory)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| {
            CistellaError::Profile(format!(
                "project name needs a directory basename: {directory}"
            ))
        })
}

/// Normalizes `container_home` before any canonicalization or derivation:
/// default-if-empty, template rejection, absolute-path check, then
/// canonicalization. Shared by literal validation and template expansion
/// so both paths reject `{{...}}` identically.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on empty-after-default impossibility
/// (unreachable), template syntax, or non-absolute paths.
fn normalize_container_home(home: &str) -> Result<String> {
    let home = if home.trim().is_empty() {
        default_container_home()
    } else {
        home.to_string()
    };
    if home.contains("{{") {
        return Err(CistellaError::Profile(
            "container_home must not contain templates".to_string(),
        ));
    }
    if !home.starts_with('/') {
        return Err(CistellaError::Profile(
            "container_home must be absolute".to_string(),
        ));
    }
    Ok(canonicalize_container_target(&home))
}

/// Template names expanded in mounts and command argv.
const TEMPLATE_CONTAINER_HOME: &str = "container-home";
const TEMPLATE_HOST_HOME: &str = "host-home";
const TEMPLATE_PROJECT_NAME: &str = "project-name";

/// Validates a project name before expansion: non-empty ASCII
/// alphanumeric plus `-_.` (agentmux precedent). Rejection, never
/// sanitization — sanitizing could silently mount the wrong tree.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on empty or out-of-charset names.
fn validate_project_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(CistellaError::Profile(format!(
            "project name must match [A-Za-z0-9-_.]+: {name}"
        )));
    }
    Ok(())
}

/// Expands `{{...}}` templates in mount triples, command argv, env
/// values, and labels values.
///
/// `project_name` is `Some` on the conduct path and `None` for
/// context-free callers: with `None`, template-free profiles pass
/// through and template-bearing profiles are a typed error (nothing
/// derives from the working directory outside conduct).
///
/// Single pass, no rescan: substituted values are never re-examined, so
/// brace-shaped values stay literal text. `container_home` itself must
/// be template-free (expansion needs the canonical home first).
///
/// # Errors
///
/// Returns `CistellaError::Profile` on unknown template names, missing
/// project context, bad project charset, missing `HOME`, or templates
/// in `container_home`.
/// Project-name input for template expansion.
#[derive(Debug, Clone, Copy)]
pub enum ProjectName<'a> {
    /// Explicit `--project-name` flag value.
    Explicit(&'a str),
    /// Derive from the canonical session directory basename, lazily —
    /// only when a `{{project-name}}` span actually expands, so
    /// template-free sessions never pay for (or fail on) derivation.
    DirectoryDefault(&'a str),
}

/// Expands `{{...}}` templates in mount triples, command argv, env
/// values, and labels values.
///
/// `project` is `Some` on the conduct path and `None` for context-free
/// callers: with `None`, template-free profiles pass through and
/// template-bearing profiles are a typed error (nothing derives from
/// the working directory outside conduct).
///
/// Order: normalize `container_home` (default-if-empty, template
/// rejection, canonicalization) so `{{container-home}}` is always the
/// canonical home; then recognize every span exactly (unknown and
/// unterminated spans error before any context, `HOME`, or charset
/// requirement); then resolve values lazily; then substitute in a
/// single pass with no rescan, so brace-shaped values stay literal.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on templates in `container_home`,
/// unknown or unterminated spans, missing project context, bad project
/// charset, or missing `HOME`.
fn expand_templates(profile: &mut Profile, project: Option<ProjectName>) -> Result<()> {
    profile.container_home = normalize_container_home(&profile.container_home)?;
    // Exact span recognition across every value first: unknown and
    // unterminated spans outrank context, HOME, and charset errors.
    let mut names: Vec<String> = Vec::new();
    for (field, value) in template_values(profile) {
        // Unwrap the inner Profile string: interpolating the error would
        // double the `profile:` display prefix.
        let parts = split_spans(value).map_err(|e| {
            let inner = match e {
                CistellaError::Profile(msg) => msg,
                other => other.to_string(),
            };
            CistellaError::Profile(format!("{inner} in {field}"))
        })?;
        for part in parts {
            if let Part::Name(name) = part
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
    }
    for name in &names {
        if !is_known_template(name) {
            return Err(CistellaError::Profile(format!(
                "unknown template {{{name}}}"
            )));
        }
    }
    // Label keys never expand (keys are lookup dimensions; expanding them
    // would destabilize matching), so any span there is a typed error.
    // Env keys need no handling: `[A-Z_][A-Z0-9_]*` cannot contain braces.
    // NOTE on the scan/substitute asymmetry: label keys appear in
    // `template_values()` so their spans are recognized (and rejected
    // here), but the substitution loop below deliberately iterates
    // values only — keys are scan-only by design, not by omission.
    for key in profile.labels.keys() {
        let parts = split_spans(key).map_err(|e| {
            let inner = match e {
                CistellaError::Profile(msg) => msg,
                other => other.to_string(),
            };
            CistellaError::Profile(format!("{inner} in label key"))
        })?;
        for part in parts {
            if let Part::Name(name) = part {
                return Err(CistellaError::Profile(format!(
                    "template {{{name}}} in label key"
                )));
            }
        }
    }
    if names.is_empty() {
        return Ok(());
    }
    let Some(project) = project else {
        return Err(CistellaError::Profile(
            "template requires project context".to_string(),
        ));
    };
    let host_home = if names.iter().any(|n| n == TEMPLATE_HOST_HOME) {
        Some(std::env::var("HOME").map_err(|_| {
            CistellaError::Profile("HOME not set for template expansion".to_string())
        })?)
    } else {
        None
    };
    let project_value = match project {
        ProjectName::Explicit(name) => {
            if names.iter().any(|n| n == TEMPLATE_PROJECT_NAME) {
                validate_project_name(name)?;
            }
            name.to_string()
        }
        ProjectName::DirectoryDefault(directory) => {
            if names.iter().any(|n| n == TEMPLATE_PROJECT_NAME) {
                let derived = default_project_name(directory)?;
                validate_project_name(&derived)?;
                derived
            } else {
                String::new()
            }
        }
    };
    let values = [
        (TEMPLATE_CONTAINER_HOME, profile.container_home.clone()),
        (TEMPLATE_HOST_HOME, host_home.unwrap_or_default()),
        (TEMPLATE_PROJECT_NAME, project_value),
    ];
    for triple in &mut profile.mounts {
        triple.host_source = substitute(&triple.host_source, &values)?;
        triple.container_target = substitute(&triple.container_target, &values)?;
    }
    if let Some(command) = &mut profile.command {
        for arg in command {
            *arg = substitute(arg, &values)?;
        }
    }
    for value in profile.env.values_mut() {
        *value = substitute(value, &values)?;
    }
    for value in profile.labels.values_mut() {
        *value = substitute(value, &values)?;
    }
    Ok(())
}

/// All template-bearing texts in a profile as (field, text) pairs:
/// triples both sides, argv, env values, labels values, and labels keys
/// (keys are scanned for fail-closed rejection, never substituted).
/// The field label exists so diagnostics can name the offense without
/// echoing the raw value (env values may carry secrets).
fn template_values(profile: &Profile) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    for triple in &profile.mounts {
        out.push(("mount host_source", triple.host_source.as_str()));
        out.push(("mount container_target", triple.container_target.as_str()));
    }
    if let Some(command) = &profile.command {
        out.extend(command.iter().map(|a| ("command argv", String::as_str(a))));
    }
    out.extend(
        profile
            .env
            .values()
            .map(|v| ("env value", String::as_str(v))),
    );
    out.extend(
        profile
            .labels
            .values()
            .map(|v| ("label value", String::as_str(v))),
    );
    out.extend(
        profile
            .labels
            .keys()
            .map(|k| ("label key", String::as_str(k))),
    );
    out
}

/// Whether a name is a known template.
fn is_known_template(name: &str) -> bool {
    matches!(
        name,
        TEMPLATE_CONTAINER_HOME | TEMPLATE_HOST_HOME | TEMPLATE_PROJECT_NAME
    )
}

/// One parsed piece of a template-bearing value.
enum Part<'a> {
    /// Literal text, copied verbatim.
    Lit(&'a str),
    /// Template name (trimmed), resolved from the value table.
    Name(String),
}

/// Splits a value into literal and template parts (exact `{{name}}`
/// spans; substituted output is never re-scanned by callers).
///
/// The unterminated-span diagnostic carries a byte offset, never the
/// raw value: callers add field context, and env values may be secrets.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on unterminated spans.
fn split_spans(value: &str) -> Result<Vec<Part<'_>>> {
    let mut parts = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        if start > 0 {
            parts.push(Part::Lit(&rest[..start]));
        }
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            let offset = value.len() - rest.len() + start;
            return Err(CistellaError::Profile(format!(
                "unterminated template span at byte {offset}"
            )));
        };
        parts.push(Part::Name(after[..end].trim().to_string()));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        parts.push(Part::Lit(rest));
    }
    Ok(parts)
}

/// Substitutes known names into a value (all names pre-validated by the
/// recognition pass; single pass, no rescan).
fn substitute(value: &str, values: &[(&str, String)]) -> Result<String> {
    let mut out = String::with_capacity(value.len());
    for part in split_spans(value)? {
        match part {
            Part::Lit(lit) => out.push_str(lit),
            Part::Name(name) => {
                let Some((_, replacement)) = values.iter().find(|(n, _)| *n == name) else {
                    // No raw value: names are bounded identifiers, but the
                    // surrounding text may be secret (see split_spans).
                    return Err(CistellaError::Profile(format!(
                        "unknown template {{{name}}}"
                    )));
                };
                out.push_str(replacement);
            }
        }
    }
    Ok(out)
}
/// Re-export for doc links.
pub use crate::mount::MountTriple as ProfileMountTriple;
