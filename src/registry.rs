//! Session registry: unit files joined with runtime state plus selectors.
//!
//! The unit-file directory is the source of truth (Quadlet `--rm` makes
//! stopped containers invisible to `podman ps`). `survey` filters with zero
//! or many selectors; `enter`/`inspect`/`terminate` resolve exactly one.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CistellaError, Result};
use crate::runtime::{query_unit_props, systemd_user_available, unquote_systemd};
use crate::session::{
    LABEL_DIRECTORY, LABEL_ID, LABEL_IDENTITY, LABEL_IMAGE, LABEL_PROFILE, RESERVED_PREFIX,
};

/// One registry row: driver labels parsed from the unit file plus liveness.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    /// Minted session id.
    pub id: String,
    /// Container name (`cistella-<id>`).
    pub container_name: String,
    /// Canonical host directory.
    pub directory: String,
    /// Profile name.
    pub profile: String,
    /// Identity label.
    pub identity: String,
    /// Resolved image digest.
    pub image: String,
    /// `systemctl show ActiveState` (`unknown` without systemd).
    pub active_state: String,
    /// Present in `podman ps --all` (container not yet reaped by `--rm`).
    pub container_present: bool,
    /// Generic (non-`cistella.*`) labels from the unit file.
    pub generic_labels: Vec<(String, String)>,
}

impl SessionRecord {
    /// Returns the value of a driver or generic label by key.
    #[must_use]
    pub fn label_value(&self, key: &str) -> Option<&str> {
        match key {
            LABEL_ID => Some(&self.id),
            LABEL_DIRECTORY => Some(&self.directory),
            LABEL_PROFILE => Some(&self.profile),
            LABEL_IDENTITY => Some(&self.identity),
            LABEL_IMAGE => Some(&self.image),
            _ => self
                .generic_labels
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str()),
        }
    }
}

/// Reads one `Label=` value from a unit file (source of truth for
/// `--rm`'d orphans whose container — and podman labels — are gone).
///
/// Values are systemd-unquoted: the unit stores `"..."` and the registry
/// sees the literal value podman received. Pure fs read, so gc phase-1
/// classification can use it without another fallible `podman inspect`.
pub(crate) fn unit_file_label(unit_file: &Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(unit_file).ok()?;
    let prefix = format!("Label={key}=");
    for line in content.lines() {
        if let Some(value) = line.strip_prefix(&prefix) {
            return Some(unquote_systemd(value.trim()));
        }
    }
    None
}

/// Lists sessions by joining the unit-file registry with
/// `systemctl show ActiveState` and `podman ps` presence.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when the unit directory is unreadable
/// in an unexpected way (missing directory yields an empty list).
pub fn list_sessions() -> Result<Vec<SessionRecord>> {
    let mut records = Vec::new();
    let Some(dir) = crate::runtime::quadlet_dir() else {
        return Ok(records);
    };
    let entries = std::fs::read_dir(&dir)
        .map(|r| r.collect::<Vec<_>>())
        .unwrap_or_default();
    let mut names: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.into_iter().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("cistella-") && name.ends_with(".container") {
            let container = name.trim_end_matches(".container").to_string();
            names.push((container, entry.path()));
        }
    }
    names.sort();
    // One `podman ps` call maps container presence for every record.
    let mut present = std::collections::HashSet::new();
    if let Ok(out) = Command::new("podman")
        .args([
            "ps",
            "--all",
            "--filter",
            "label=cistella.id",
            "--format",
            "{{.Names}}",
        ])
        .output()
        && out.status.success()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() {
                present.insert(trimmed);
            }
        }
    }
    let systemd = systemd_user_available();
    for (container, path) in names {
        let id = unit_file_label(&path, LABEL_ID).unwrap_or_else(|| {
            container
                .strip_prefix("cistella-")
                .unwrap_or(&container)
                .to_string()
        });
        let active_state = if systemd {
            let service = format!("{container}.service");
            query_unit_props(&service)
                .get("ActiveState")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string())
        } else {
            "unknown".to_string()
        };
        let mut generic_labels = Vec::new();
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("Label=")
                    && let Some((key, value)) = rest.split_once('=')
                    && !key.starts_with(RESERVED_PREFIX)
                {
                    generic_labels.push((key.to_string(), unquote_systemd(value)));
                }
            }
        }
        records.push(SessionRecord {
            directory: unit_file_label(&path, LABEL_DIRECTORY).unwrap_or_default(),
            profile: unit_file_label(&path, LABEL_PROFILE).unwrap_or_default(),
            identity: unit_file_label(&path, LABEL_IDENTITY).unwrap_or_default(),
            image: unit_file_label(&path, LABEL_IMAGE).unwrap_or_default(),
            container_present: present.contains(&container),
            generic_labels,
            container_name: container,
            active_state,
            id,
        });
    }
    Ok(records)
}

/// Filters records for `survey`: zero or many selectors as ANDed filters.
#[must_use]
pub fn filter_records<'a>(
    records: &'a [SessionRecord],
    directory: Option<&str>,
    labels: &[(String, String)],
) -> Vec<&'a SessionRecord> {
    records
        .iter()
        .filter(|r| {
            if let Some(dir) = directory
                && r.directory != dir
            {
                return false;
            }
            for (key, value) in labels {
                if r.label_value(key) != Some(value.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Resolves exactly one selector for `enter`/`inspect`/`terminate`.
///
/// Exactly one of the positional `<id>` unique prefix, `--directory`, or
/// `--label` forms must be given (mixing is usage error); zero or multiple
/// matches are typed refusals listing candidates.
///
/// # Errors
///
/// Returns `CistellaError::Selector` on usage errors, empty matches, or
/// ambiguous matches.
pub fn resolve_exact(
    records: &[SessionRecord],
    id_prefix: Option<&str>,
    directory: Option<&str>,
    labels: &[(String, String)],
) -> Result<SessionRecord> {
    let forms = usize::from(id_prefix.is_some())
        + usize::from(directory.is_some())
        + usize::from(!labels.is_empty());
    if forms != 1 {
        return Err(CistellaError::Selector(
            "pass exactly one of <id>, --directory, --label".to_string(),
        ));
    }
    let matches: Vec<&SessionRecord> = if let Some(prefix) = id_prefix {
        records
            .iter()
            .filter(|r| r.id.starts_with(prefix))
            .collect()
    } else if let Some(dir) = directory {
        records.iter().filter(|r| r.directory == dir).collect()
    } else {
        records
            .iter()
            .filter(|r| {
                labels
                    .iter()
                    .all(|(k, v)| r.label_value(k) == Some(v.as_str()))
            })
            .collect()
    };
    if matches.is_empty() {
        let names: Vec<&str> = records.iter().map(|r| r.container_name.as_str()).collect();
        return Err(CistellaError::Selector(format!(
            "no session matches; candidates: {}",
            names.join(", ")
        )));
    }
    if matches.len() > 1 {
        let names: Vec<&str> = matches.iter().map(|r| r.container_name.as_str()).collect();
        return Err(CistellaError::Selector(format!(
            "ambiguous selector; candidates: {}",
            names.join(", ")
        )));
    }
    Ok(matches[0].clone())
}
