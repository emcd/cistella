//! Smoke test for the cistella crate.
//!
//! Ensures the binary has at least one runnable test so the
//! `cargo-nextest` pre-commit hook passes on the skeleton. Once
//! real tests exist they should live alongside this file or
//! under `tests/integration/` per the project's testing
//! conventions.

use cistella::version;

#[test]
fn version_is_non_empty() {
    assert!(!version().is_empty());
}
