//! Isolator backends behind the framework trait.
//!
//! `podman` is the first (and currently only) backend. The
//! deterministic protocol peer joins this module as a
//! fault-injecting test-double, never a lifecycle prover.

pub mod client;
pub mod client_verdicts;
pub mod close;
pub mod corroboration;
pub mod dispatch;
pub mod hook;
pub mod podman;
pub(crate) mod podman_await;
pub mod quadlet;
pub mod quadlet_gc;
pub mod wire;
