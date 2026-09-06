//! Unit tests entry point.
//!
//! Each submodule pulls in a `.rs` file from `tests/unit/` and is
//! compiled into this single test binary. Cargo's autotests discovery
//! picks up `tests/unit.rs`; the `#[path]` attribute directs each
//! submodule to its file in the subdirectory.

#[path = "unit/identity.rs"]
mod identity;
#[path = "unit/mount.rs"]
mod mount;
#[path = "unit/profile_resolution.rs"]
mod profile_resolution;
#[path = "unit/runtime.rs"]
mod runtime;
#[path = "unit/version.rs"]
mod version;
