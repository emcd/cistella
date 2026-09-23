//! Policy lattice: user `policies.toml`, compiled defaults, evaluation.
//!
//! Severity (`suppressible`/`inviolable`) × scope (`universal`/
//! `on-extensions`) enforced at two gates: pre-merge contribution
//! admissibility and pre-exec enforcement. Precedence is site, then
//! user, then per-profile declarations, then compiled defaults, with
//! tighten-only movement downward. Exact-name acknowledgements live
//! in user policy only (never profiles, never a conduct flag).
//! Diagnostics name variables, never values.
//!
//! File contract (`$XDG_CONFIG_HOME/cistella/policies.toml`,
//! `~/.config` fallback; absent means compiled defaults only):
//! `format_version = 1`; `[[denials]]` entries `{pattern (regex,
//! compiled once at load), severity, scope}`; `[[acknowledgements]]`
//! entries `{name (exact, env-name grammar)}`. Unknown fields,
//! duplicate entries, bad regexes, unknown versions, and a user
//! `inviolable × universal` entry refuse fail-closed before planning:
//! that cell is site-authority only, and admitting it would let user
//! policy impersonate site policy.
//!
//! Compiled defaults are suppressible only. The shipped default
//! mirrors the 0.1.x push-credential guard (`GITHUB_TOKEN`,
//! `GH_TOKEN`, `GITHUB_PAT`, case-insensitive) widened to universal
//! scope per the ratified lattice; 0.1.1 `environment-acceptances`
//! entries are grandfathered against compiled-default rules only.
//!
//! The site layer is seam-only in 0.2.0 (profiles/13): no compiled
//! default masquerades as site authority. [`PolicySet`] carries an
//! (empty) site rule list as the hook where the root-owned source
//! binds later; user policy can never weaken it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Deserialize;

use crate::error::{CistellaError, Result};
use crate::framework::contract::{PolicyClaim, Provenance, Scope, Severity};

/// User policy file format version (only version 1 exists).
pub const POLICY_FORMAT_VERSION: u32 = 1;

/// Compiled-default rule: token-shaped push credentials, any provenance.
const DEFAULT_TOKEN_PATTERN: &str = "(?i)^(GITHUB_TOKEN|GH_TOKEN|GITHUB_PAT)$";

/// One `[[denials]]` entry in user policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenialEntry {
    /// Regex matched against variable names (compiled once at load).
    pub pattern: String,
    /// Claimed severity.
    pub severity: Severity,
    /// Claimed scope.
    pub scope: Scope,
}

/// One `[[acknowledgements]]` entry in user policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgementEntry {
    /// Exact variable name acknowledged (env-name grammar).
    pub name: String,
}

/// On-disk user policy file shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    /// Must be [`POLICY_FORMAT_VERSION`].
    pub format_version: u32,
    /// Denial rules (default empty).
    #[serde(default)]
    pub denials: Vec<DenialEntry>,
    /// Exact-name acknowledgements (default empty).
    #[serde(default)]
    pub acknowledgements: Vec<AcknowledgementEntry>,
}

/// One compiled rule: regex plus lattice position plus layer.
#[derive(Debug, Clone)]
struct Rule {
    /// Compiled name pattern.
    pattern: Regex,
    /// Pattern source text (diagnostics and tighten-only comparison).
    source: String,
    /// Lattice severity.
    severity: Severity,
    /// Lattice scope.
    scope: Scope,
    /// True for compiled defaults (grandfathering + tighten-only).
    is_default: bool,
}

/// Loaded policy: site (seam), user, and compiled-default rules plus
/// exact-name acknowledgements and the file content hash (which joins
/// the plan baseline binding).
#[derive(Debug, Clone)]
pub struct PolicySet {
    site: Vec<Rule>,
    user: Vec<Rule>,
    defaults: Vec<Rule>,
    acknowledgements: HashSet<String>,
    /// Hex hash of the file bytes, or of the empty string when absent.
    pub source_hash: String,
}

impl PolicySet {
    /// Loads user policy, refusing fail-closed before any planning.
    ///
    /// `config_dir` overrides the directory lookup (tests); `None`
    /// uses `$XDG_CONFIG_HOME/cistella` with a `~/.config` fallback.
    /// An absent file means compiled defaults only.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on malformed TOML, unknown
    /// fields or versions, duplicate entries, bad regexes, a user
    /// `inviolable × universal` entry, or a user rule that weakens a
    /// compiled default (tighten-only downward).
    pub fn load(config_dir: Option<&Path>) -> Result<Self> {
        let path = policy_path(config_dir);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(CistellaError::Contract(format!(
                    "read {}: {e}",
                    path.display()
                )));
            }
        };
        Self::parse(&bytes)
    }

    /// Parses policy file bytes (load's pure half, unit-testable).
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on any file-contract violation.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        use sha2::{Digest, Sha256};
        let source_hash = format!("{:x}", Sha256::digest(bytes));
        if bytes.is_empty() {
            return Ok(Self::defaults_only(source_hash));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| CistellaError::Contract("policies.toml is not UTF-8".to_string()))?;
        let file: PolicyFile = toml::from_str(text)
            .map_err(|e| CistellaError::Contract(format!("policies.toml: {e}")))?;
        if file.format_version != POLICY_FORMAT_VERSION {
            return Err(CistellaError::Contract(format!(
                "policies.toml format_version {} unsupported (want {POLICY_FORMAT_VERSION})",
                file.format_version
            )));
        }
        let mut seen_denials = HashSet::new();
        let mut user = Vec::with_capacity(file.denials.len());
        for entry in &file.denials {
            let key = format!("{}\0{:?}\0{:?}", entry.pattern, entry.severity, entry.scope);
            if !seen_denials.insert(key) {
                return Err(CistellaError::Contract(format!(
                    "policies.toml duplicate denial: {}",
                    entry.pattern
                )));
            }
            if entry.severity == Severity::Inviolable && entry.scope == Scope::Universal {
                return Err(CistellaError::Contract(
                    "policies.toml: inviolable universal is site-authority only".to_string(),
                ));
            }
            let pattern = Regex::new(&entry.pattern).map_err(|e| {
                CistellaError::Contract(format!("policies.toml bad pattern {}: {e}", entry.pattern))
            })?;
            user.push(Rule {
                pattern,
                source: entry.pattern.clone(),
                severity: entry.severity,
                scope: entry.scope,
                is_default: false,
            });
        }
        let mut acknowledgements = HashSet::new();
        for entry in &file.acknowledgements {
            crate::profile::validate_env_name(&entry.name, "acknowledgement").map_err(|e| {
                CistellaError::Contract(format!("policies.toml bad acknowledgement: {e}"))
            })?;
            if !acknowledgements.insert(entry.name.clone()) {
                return Err(CistellaError::Contract(format!(
                    "policies.toml duplicate acknowledgement: {}",
                    entry.name
                )));
            }
        }
        let defaults = default_rules();
        check_tighten_only(&user, &defaults)?;
        Ok(Self {
            site: Vec::new(),
            user,
            defaults,
            acknowledgements,
            source_hash,
        })
    }

    /// Compiled defaults only (absent file).
    fn defaults_only(source_hash: String) -> Self {
        Self {
            site: Vec::new(),
            user: Vec::new(),
            defaults: default_rules(),
            acknowledgements: HashSet::new(),
            source_hash,
        }
    }
    /// Evaluates one variable against the lattice plus upheld
    /// transaction claims.
    ///
    /// `acceptances` carries shipped 0.1.1 `environment-acceptances`
    /// entries, grandfathered against compiled-default rules only: an
    /// exact acceptance is not itself an acknowledgement, user/site
    /// suppressible denials still require explicit acknowledgement,
    /// and later rules apply normally. Upheld claims enforce like
    /// rules without grandfathering (they are transaction scope, not
    /// defaults). Diagnostics name the variable, its scope, and its
    /// severity — never its value.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on the first refusing rule.
    pub fn evaluate(
        &self,
        name: &str,
        provenance: &Provenance,
        acceptances: &HashSet<String>,
        claims: &[UpheldClaim],
    ) -> Result<()> {
        for rule in self.site.iter().chain(&self.user).chain(&self.defaults) {
            if !rule_applies(rule, name, provenance) {
                continue;
            }
            if rule.is_default && acceptances.contains(name) {
                continue;
            }
            if rule.severity == Severity::Suppressible && self.acknowledgements.contains(name) {
                continue;
            }
            return Err(denial(name, &rule.severity, &rule.scope));
        }
        for claim in claims {
            if !claim.pattern.is_match(name) {
                continue;
            }
            if claim.scope == Scope::OnExtensions && !matches!(provenance, Provenance::Extension(_))
            {
                continue;
            }
            if claim.severity == Severity::Suppressible && self.acknowledgements.contains(name) {
                continue;
            }
            return Err(denial(name, &claim.severity, &claim.scope));
        }
        Ok(())
    }

    /// Partitions transaction claims against the lattice.
    ///
    /// Each claim compiles (bad regex refuses the whole transaction),
    /// must not impersonate site authority (`inviolable × universal`
    /// refuses whole), and must match at least one contributed name
    /// (a claim reaching beyond its transaction's contributions is
    /// overreach and refuses whole). A claim weaker than an applicable
    /// same-pattern rule is discarded with a typed diagnostic while
    /// the stricter rule governs; the transaction proceeds.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` refusing the whole
    /// transaction on malformed, impersonating, or overreaching claims.
    pub fn partition_claims(
        &self,
        claims: &[PolicyClaim],
        contributed: &[String],
    ) -> Result<(Vec<UpheldClaim>, Vec<String>)> {
        let mut upheld = Vec::new();
        let mut diagnostics = Vec::new();
        for claim in claims {
            if claim.pattern.is_empty() {
                return Err(CistellaError::Contract(
                    "policy claim pattern must not be empty".to_string(),
                ));
            }
            if claim.severity == Severity::Inviolable && claim.scope == Scope::Universal {
                return Err(CistellaError::Contract(format!(
                    "policy claim impersonates site authority: {}",
                    claim.pattern
                )));
            }
            let pattern = Regex::new(&claim.pattern).map_err(|e| {
                CistellaError::Contract(format!("bad policy claim pattern {}: {e}", claim.pattern))
            })?;
            if !contributed.iter().any(|name| pattern.is_match(name)) {
                return Err(CistellaError::Contract(format!(
                    "policy claim reaches beyond its transaction: {}",
                    claim.pattern
                )));
            }
            let weaker_than = self
                .site
                .iter()
                .chain(&self.user)
                .chain(&self.defaults)
                .filter(|rule| rule.source == claim.pattern)
                .any(|rule| {
                    severity_rank(rule.severity) > severity_rank(claim.severity)
                        || scope_width(rule.scope) > scope_width(claim.scope)
                });
            if weaker_than {
                diagnostics.push(format!(
                    "discards weakening claim for {} ({:?} {:?}): stricter rule governs",
                    claim.pattern, claim.severity, claim.scope
                ));
                continue;
            }
            upheld.push(UpheldClaim::uphold(claim, pattern)?);
        }
        Ok((upheld, diagnostics))
    }
}

/// One upheld transaction claim: compiled pattern plus lattice position.
///
/// Enforced like a rule for the transaction's contributions, without
/// grandfathering and without persistence beyond the transaction.
#[derive(Debug, Clone)]
pub struct UpheldClaim {
    /// Compiled claim pattern.
    pub pattern: Regex,
    /// Claimed severity.
    pub severity: Severity,
    /// Claimed scope.
    pub scope: Scope,
}

impl UpheldClaim {
    /// Compiles one partitioned claim: the single conversion point
    /// from wire shape to enforced shape, so future fields land in
    /// one place.
    ///
    /// # Errors
    ///
    /// Returns `CistellaError::Contract` on an uncompilable pattern.
    fn uphold(claim: &PolicyClaim, pattern: Regex) -> Result<Self> {
        Ok(Self {
            pattern,
            severity: claim.severity,
            scope: claim.scope,
        })
    }
}
/// Compiled-default ruleset (suppressible only, by construction).
fn default_rules() -> Vec<Rule> {
    vec![Rule {
        pattern: Regex::new(DEFAULT_TOKEN_PATTERN).expect("default pattern compiles"),
        source: DEFAULT_TOKEN_PATTERN.to_string(),
        severity: Severity::Suppressible,
        scope: Scope::Universal,
        is_default: true,
    }]
}

/// True when a rule reaches this variable.
fn rule_applies(rule: &Rule, name: &str, provenance: &Provenance) -> bool {
    if !rule.pattern.is_match(name) {
        return false;
    }
    match rule.scope {
        Scope::Universal => true,
        Scope::OnExtensions => matches!(provenance, Provenance::Extension(_)),
    }
}

/// Value-free denial diagnostic: name, scope, severity.
fn denial(name: &str, severity: &Severity, scope: &Scope) -> CistellaError {
    CistellaError::Contract(format!("policy refuses {name}: {scope:?} {severity:?}"))
}

/// Severity rank for weakening comparison.
fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Suppressible => 0,
        Severity::Inviolable => 1,
    }
}

/// Scope width for weakening comparison.
fn scope_width(scope: Scope) -> u8 {
    match scope {
        Scope::OnExtensions => 0,
        Scope::Universal => 1,
    }
}

/// Tighten-only downward: a user rule with the same pattern string as
/// a default must not narrow scope (universal → on-extensions).
/// Severity cannot drop below suppressible (the default floor), and a
/// user `inviolable × universal` never reaches this check (parse
/// refuses it as site impersonation).
///
/// # Errors
///
/// Returns `CistellaError::Contract` naming the weakening rule.
fn check_tighten_only(user: &[Rule], defaults: &[Rule]) -> Result<()> {
    for candidate in user {
        for baseline in defaults {
            if candidate.source != baseline.source {
                continue;
            }
            if baseline.scope == Scope::Universal && candidate.scope == Scope::OnExtensions {
                return Err(CistellaError::Contract(format!(
                    "policies.toml weakens default rule: {}",
                    candidate.source
                )));
            }
        }
    }
    Ok(())
}

/// Resolves the user policy path: override, `$XDG_CONFIG_HOME`, or
/// `~/.config` fallback.
fn policy_path(config_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = config_dir {
        return dir.join("policies.toml");
    }
    let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_default();
    if !base.is_empty() {
        return PathBuf::from(base).join("cistella/policies.toml");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/cistella/policies.toml")
}

/// Loads the acknowledgement-relevant names: shipped acceptances.
///
/// Grandfathering input for [`PolicySet::evaluate`]: pure
/// convenience over the profile's `environment-acceptances` list —
/// exactly the names listed, neither curated nor filtered.
#[must_use]
pub fn acceptance_set(names: &[String]) -> HashSet<String> {
    names.iter().cloned().collect()
}

/// Evaluates a batch of names (contributions or assignments),
/// failing on the first refusal.
///
/// # Errors
///
/// Returns `CistellaError::Contract` on the first refusing variable.
pub fn evaluate_all(
    policy: &PolicySet,
    names: &[(String, Provenance)],
    acceptances: &HashSet<String>,
    claims: &[UpheldClaim],
) -> Result<()> {
    for (name, provenance) in names {
        policy.evaluate(name, provenance, acceptances, claims)?;
    }
    Ok(())
}

/// Serializes the policy-relevant evaluation trail (names only).
#[must_use]
pub fn evaluation_trail(names: &[(String, Provenance)]) -> HashMap<String, String> {
    names
        .iter()
        .map(|(name, provenance)| {
            let source = match provenance {
                Provenance::Profile => "profile",
                Provenance::Extension(_) => "extension",
            };
            (name.clone(), source.to_string())
        })
        .collect()
}
