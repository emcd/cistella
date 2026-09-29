//! Framework lifecycle/capability contract.
//!
//! Spine phases, typed prepare-transaction merge, deadline ownership,
//! and reconciliation identity. The isolator trait builds
//! on these types; nothing here knows Podman.

pub mod conduct;
pub mod contract;
pub mod credentials;
pub mod discovery;
pub mod fdpass;
pub mod guest;
pub mod hooks;
pub mod isolator;
pub mod policy;
pub mod prepare;
pub mod protocol;
pub mod registry;
pub mod signals;
pub mod stream;
