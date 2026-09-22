//! Template engine: qualified spans, supplements, environment.
//!
//! Spans (`{{context:name}}`) resolve per provider context — `core` is
//! driver-resolved, `supplement` is caller-provided, `environment` is the
//! host process environment under a compile-time allowlist with a hard
//! credential deny. [`expand_templates`] runs the `container-home` early
//! phase, exact span recognition, lazy value resolution, and single-pass
//! substitution with no rescan.

use crate::error::{CistellaError, Result};
use crate::profile::{Profile, default_project_name, normalize_container_home};

/// Template names expanded in mounts and command argv.
const CORE_CONTAINER_HOME: &str = "container-home";
const CORE_HOST_HOME: &str = "host-home";
const CORE_PROJECT_NAME: &str = "project-name";

/// Template span contexts (`{{context:name}}`): the context names the
/// value provider, so every span's provenance reads at a glance. `core`
/// means driver-resolved (flag values, derived defaults, canonical
/// paths) — not driver-static: per-invocation values the driver derives
/// still live here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateContext {
    /// Driver-resolved values (`container-home`, `host-home`,
    /// `project-name`).
    Core,
    /// Caller-provided values (`--supplement k=v`).
    Supplement,
    /// Host process environment (compile-time allowlist, hard deny).
    Environment,
}

impl TemplateContext {
    /// Returns the context spelling used inside spans.
    fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Supplement => "supplement",
            Self::Environment => "environment",
        }
    }
}

/// A classified template span: provider context plus name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClassifiedSpan {
    /// Value provider named by the span.
    context: TemplateContext,
    /// Name within the context (trimmed, non-empty).
    name: String,
}

/// Classifies a raw span interior (`{{...}}` contents, trimmed) into a
/// provider context and name.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on bare spans (no `:` separator),
/// unknown contexts, or empty names.
fn classify_span(inner: &str) -> Result<ClassifiedSpan> {
    let Some((context_raw, name_raw)) = inner.split_once(':') else {
        return Err(CistellaError::Profile(format!(
            "bare template span {{{{{inner}}}}}; qualify with a context (core:, supplement:, environment:)"
        )));
    };
    let context = match context_raw.trim() {
        "core" => TemplateContext::Core,
        "supplement" => TemplateContext::Supplement,
        "environment" => TemplateContext::Environment,
        unknown => {
            return Err(CistellaError::Profile(format!(
                "unknown template context {unknown}"
            )));
        }
    };
    let name = name_raw.trim().to_string();
    if name.is_empty() {
        return Err(CistellaError::Profile(format!(
            "empty template name in context {}",
            context.as_str()
        )));
    }
    Ok(ClassifiedSpan { context, name })
}

/// Caller-provided template values (`--supplement k=v`, last-wins on
/// duplicate keys).
///
/// Values stay opaque until substitution: no charset gate applies to the
/// raw value, so a supplement may supply a whole path. Each fully
/// substituted sink applies its existing validator. Supplements are
/// trusted, non-secret caller metadata; callers must not bridge ambient
/// secrets into them.
#[derive(Debug, Clone, Default)]
pub struct Supplements {
    /// Deduplicated pairs in first-seen key order.
    pairs: Vec<(String, String)>,
}

impl Supplements {
    /// Builds the table from parsed pairs (later pairs win on duplicates).
    #[must_use]
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        let mut deduped: Vec<(String, String)> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            if let Some(slot) = deduped.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value;
            } else {
                deduped.push((key, value));
            }
        }
        Self { pairs: deduped }
    }

    /// Returns the value for a referenced supplement name, if supplied.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Parses one `--supplement k=v` argument (key constraints enforced).
///
/// # Errors
///
/// Returns `CistellaError::Profile` on missing `=`, empty keys,
/// `cistella`-reserved keys, or out-of-charset keys.
pub fn parse_supplement_arg(arg: &str) -> Result<(String, String)> {
    let Some((key, value)) = arg.split_once('=') else {
        return Err(CistellaError::Profile(format!(
            "supplement must be k=v: {arg}"
        )));
    };
    validate_supplement_key(key)?;
    Ok((key.to_string(), value.to_string()))
}

/// Validates a supplement key: non-empty, never `cistella` nor beginning
/// with `cistella.`, conservative ASCII (`[A-Za-z0-9_. -]+`) so
/// diagnostics stay readable.
fn validate_supplement_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(CistellaError::Profile(
            "supplement key must not be empty".to_string(),
        ));
    }
    if key == "cistella" || key.starts_with("cistella.") {
        return Err(CistellaError::Profile(format!(
            "supplement key must not use the cistella namespace: {key}"
        )));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
    {
        return Err(CistellaError::Profile(format!(
            "supplement key must match [A-Za-z0-9_. -]+: {key}"
        )));
    }
    Ok(())
}

/// Compile-time allowlist for `{{environment:*}}`: the sole concrete
/// consumer is the `container-home` showcase. Extended only by
/// code-reviewed change — never by runtime configuration, which would
/// collapse allowlist-by-default for novel credential names.
const ENV_ALLOWLIST: &[&str] = &["HOME"];

/// Whether an environment name is credential-shaped (case-insensitive):
/// exact `SSH_AUTH_SOCK`, `*_TOKEN` / `*_SECRET` / `*_KEY` /
/// `*_PASSWORD` suffixes, or `*CREDENTIAL*` infix. Deny overrides the
/// allowlist: templates must never exfiltrate ambient credentials.
fn environment_hard_denied(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "SSH_AUTH_SOCK"
        || ["_TOKEN", "_SECRET", "_KEY", "_PASSWORD"]
            .iter()
            .any(|suffix| upper.ends_with(suffix))
        || upper.contains("CREDENTIAL")
}

/// Resolves one referenced `{{environment:NAME}}` span in fixed order:
/// hard-deny check, then allowlist, then host lookup. Only referenced
/// names are ever looked up (no environment dump). Diagnostics name
/// only the variable, never its value.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on out-of-charset names, hard-denied
/// names, names absent from the allowlist, or unset variables.
fn resolve_environment(name: &str) -> Result<String> {
    if name.is_empty()
        || !name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(CistellaError::Profile(format!(
            "invalid environment name: {name}"
        )));
    }
    if environment_hard_denied(name) {
        return Err(CistellaError::Profile(format!(
            "environment {name} refused (credential-shaped)"
        )));
    }
    if !ENV_ALLOWLIST.contains(&name) {
        return Err(CistellaError::Profile(format!(
            "environment {name} is not allowlisted (allowlist is compile-time; extend by code-reviewed change)"
        )));
    }
    std::env::var(name)
        .map_err(|_| CistellaError::Profile(format!("environment {name} is not set")))
}

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

/// Project-name input for template expansion.
#[derive(Debug, Clone, Copy)]
pub enum ProjectName<'a> {
    /// Explicit `--project-name` flag value.
    Explicit(&'a str),
    /// Derive from the canonical session directory basename, lazily —
    /// only when a `{{core:project-name}}` span actually expands, so
    /// template-free sessions never pay for (or fail on) derivation.
    DirectoryDefault(&'a str),
}

/// Early `container-home` expansion phase (fifth sink): resolves
/// allowlisted `{{environment:*}}` and supplied `{{supplement:*}}` spans
/// before canonicalization, so `container-home =
/// '{{environment:HOME}}'` stays portable across seats.
/// `{{core:*}}` spans are typed cycle errors (self-reference:
/// `core:container-home` derives from this very field).
/// Template-free literals pass through unchanged.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on unterminated, bare, or
/// unknown-context spans, self-references, or environment/supplement
/// resolution failures.
fn expand_early_home(home: &str, supplements: &Supplements) -> Result<String> {
    let mut out = String::with_capacity(home.len());
    for part in split_spans(home).map_err(|e| {
        let inner = match e {
            CistellaError::Profile(msg) => msg,
            other => other.to_string(),
        };
        CistellaError::Profile(format!("{inner} in container-home"))
    })? {
        match part {
            Part::Lit(lit) => out.push_str(lit),
            Part::Name(raw) => {
                let span = classify_span(&raw).map_err(|e| {
                    let inner = match e {
                        CistellaError::Profile(msg) => msg,
                        other => other.to_string(),
                    };
                    CistellaError::Profile(format!("{inner} in container-home"))
                })?;
                match span.context {
                    TemplateContext::Core => {
                        return Err(CistellaError::Profile(format!(
                            "self-referential template {{{{{}}}}} in container-home",
                            raw
                        )));
                    }
                    TemplateContext::Supplement => {
                        let Some(value) = supplements.get(&span.name) else {
                            return Err(CistellaError::Profile(format!(
                                "unknown supplement: {}",
                                span.name
                            )));
                        };
                        out.push_str(value);
                    }
                    TemplateContext::Environment => {
                        out.push_str(&resolve_environment(&span.name)?);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Expands `{{context:name}}` templates in mount triples, command argv,
/// env values, and labels values, after the `container-home` early
/// phase.
///
/// `project` is `Some` on the conduct path and `None` for
/// context-free callers: with `None`, template-free profiles pass
/// through and template-bearing profiles are a typed error (nothing
/// derives from the working directory outside conduct).
///
/// Order: early `container-home` expansion, then `container_home`
/// normalization (default-if-empty, canonicalization) so
/// `{{core:container-home}}` is always the canonical home; then exact
/// recognition of every span (unterminated, bare, unknown-context, and
/// unknown-core spans error before any supplement, environment, `HOME`,
/// or charset requirement); then lazy value resolution in first-seen
/// order; then substitution in a single pass with no rescan, so
/// brace-shaped values stay literal.
///
/// # Errors
///
/// Returns `CistellaError::Profile` on early-home failures, unknown or
/// unterminated spans, missing project context, bad project charset,
/// or missing `HOME`.
pub(crate) fn expand_templates(
    profile: &mut Profile,
    project: Option<ProjectName<'_>>,
    supplements: &Supplements,
) -> Result<()> {
    profile.container_home = expand_early_home(&profile.container_home, supplements)?;
    profile.container_home = normalize_container_home(&profile.container_home)?;
    // Exact span recognition across every value first: structural span
    // errors outrank supplement, environment, HOME, and charset errors.
    let mut spans: Vec<ClassifiedSpan> = Vec::new();
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
            if let Part::Name(raw) = part {
                let span = classify_span(&raw).map_err(|e| {
                    let inner = match e {
                        CistellaError::Profile(msg) => msg,
                        other => other.to_string(),
                    };
                    CistellaError::Profile(format!("{inner} in {field}"))
                })?;
                if !spans.contains(&span) {
                    spans.push(span);
                }
            }
        }
    }
    // Unknown core names are structural: they outrank every resolution
    // error, matching the old unknown-template-first discipline.
    for span in &spans {
        if span.context == TemplateContext::Core && !is_known_core_template(&span.name) {
            return Err(CistellaError::Profile(format!(
                "unknown core template: {}",
                span.name
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
            if let Part::Name(raw) = part {
                return Err(CistellaError::Profile(format!(
                    "template {{{{{raw}}}}} in label key"
                )));
            }
        }
    }
    if spans.is_empty() {
        return Ok(());
    }
    let Some(project) = project else {
        return Err(CistellaError::Profile(
            "template requires project context".to_string(),
        ));
    };
    // Lazy resolution in first-seen span order: the first unresolvable
    // span reports, and unreferenced contexts are never consulted (no
    // HOME read, no basename derivation, no environment lookup).
    let mut values: Vec<(ClassifiedSpan, String)> = Vec::with_capacity(spans.len());
    let mut project_value: Option<String> = None;
    let mut host_home_value: Option<String> = None;
    for span in &spans {
        if values.iter().any(|(known, _)| known == span) {
            continue;
        }
        let value = match span.context {
            TemplateContext::Core if span.name == CORE_CONTAINER_HOME => {
                profile.container_home.clone()
            }
            TemplateContext::Core if span.name == CORE_HOST_HOME => {
                if host_home_value.is_none() {
                    host_home_value = Some(std::env::var("HOME").map_err(|_| {
                        CistellaError::Profile("HOME not set for template expansion".to_string())
                    })?);
                }
                host_home_value.clone().unwrap_or_default()
            }
            TemplateContext::Core if span.name == CORE_PROJECT_NAME => {
                if project_value.is_none() {
                    project_value = Some(resolve_project_value(project)?);
                }
                project_value.clone().unwrap_or_default()
            }
            TemplateContext::Core => {
                // Defensive: the structural pass above rejects every
                // unknown core name, so this is unreachable without a
                // logic change; report identically rather than panic.
                return Err(CistellaError::Profile(format!(
                    "unknown core template: {}",
                    span.name
                )));
            }
            TemplateContext::Supplement => {
                let Some(value) = supplements.get(&span.name) else {
                    return Err(CistellaError::Profile(format!(
                        "unknown supplement: {}",
                        span.name
                    )));
                };
                value.to_string()
            }
            TemplateContext::Environment => resolve_environment(&span.name)?,
        };
        values.push((span.clone(), value));
    }
    for triple in &mut profile.mounts {
        triple.host_source = substitute(&triple.host_source, &values)?;
        triple.container_target = substitute(&triple.container_target, &values)?;
    }
    if let Some(command) = &mut profile.command {
        for arg in command {
            *arg = substitute(arg, &values)?;
        }
    }
    for value in profile.environment_assignments.values_mut() {
        *value = substitute(value, &values)?;
    }
    for value in profile.labels.values_mut() {
        *value = substitute(value, &values)?;
    }
    Ok(())
}

/// Resolves the `{{core:project-name}}` value: explicit flags validate
/// directly, directory defaults derive the basename first. Called only
/// when a project span actually expands (lazy).
fn resolve_project_value(project: ProjectName<'_>) -> Result<String> {
    match project {
        ProjectName::Explicit(name) => {
            validate_project_name(name)?;
            Ok(name.to_string())
        }
        ProjectName::DirectoryDefault(directory) => {
            let derived = default_project_name(directory)?;
            validate_project_name(&derived)?;
            Ok(derived)
        }
    }
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
            .environment_assignments
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

/// Whether a name is a known `core:` template.
fn is_known_core_template(name: &str) -> bool {
    matches!(
        name,
        CORE_CONTAINER_HOME | CORE_HOST_HOME | CORE_PROJECT_NAME
    )
}

/// One parsed piece of a template-bearing value.
enum Part<'a> {
    /// Literal text, copied verbatim.
    Lit(&'a str),
    /// Raw span interior (trimmed), classified at recognition.
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

/// Substitutes classified spans into a value (all spans pre-validated
/// by the recognition pass; single pass, no rescan).
fn substitute(value: &str, values: &[(ClassifiedSpan, String)]) -> Result<String> {
    let mut out = String::with_capacity(value.len());
    for part in split_spans(value)? {
        match part {
            Part::Lit(lit) => out.push_str(lit),
            Part::Name(raw) => {
                // Pre-validated by the recognition pass; classify again
                // for the lookup key, reporting identically if reached.
                let span = classify_span(&raw)?;
                let Some((_, replacement)) = values.iter().find(|(known, _)| *known == span) else {
                    // No raw value: names are bounded identifiers, but the
                    // surrounding text may be secret (see split_spans).
                    return Err(CistellaError::Profile(format!(
                        "unknown template {{{{{raw}}}}}"
                    )));
                };
                out.push_str(replacement);
            }
        }
    }
    Ok(out)
}
