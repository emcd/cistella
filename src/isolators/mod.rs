//! Isolator backends behind the framework trait.
//!
//! `podman` is the first (and currently only) backend. The
//! deterministic protocol peer (task 3.1) joins this module as a
//! fault-injecting test-double, never a lifecycle prover.

pub mod client;
pub mod close;
pub mod dispatch;
pub mod podman;
pub mod quadlet;
pub mod wire;
