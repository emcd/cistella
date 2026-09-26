//! Unit tests entry point.
//!
//! Each submodule pulls in a `.rs` file from `tests/unit/` and is
//! compiled into this single test binary. Cargo's autotests discovery
//! picks up `tests/unit.rs`; the `#[path]` attribute directs each
//! submodule to its file in the subdirectory.

#[path = "unit/cli_args.rs"]
mod cli_args;
#[path = "unit/conduct_eval.rs"]
mod conduct_eval;
#[path = "unit/credentials.rs"]
mod credentials;
#[path = "unit/discovery.rs"]
mod discovery;
#[path = "unit/environment_acceptances.rs"]
mod environment_acceptances;
#[path = "unit/fdpass.rs"]
mod fdpass;
#[path = "unit/framework_contract.rs"]
mod framework_contract;
#[path = "unit/identity.rs"]
mod identity;
#[path = "unit/isolator_trait.rs"]
mod isolator_trait;
#[path = "unit/lock_teardown.rs"]
mod lock_teardown;
#[path = "unit/mount.rs"]
mod mount;
#[path = "unit/policy_prepare.rs"]
mod policy_prepare;
#[path = "unit/prepare.rs"]
mod prepare;
#[path = "unit/profile_resolution.rs"]
mod profile_resolution;
#[path = "unit/protocol.rs"]
mod protocol;
#[path = "unit/runtime.rs"]
mod runtime;
#[path = "unit/stream.rs"]
mod stream;
#[path = "unit/terminal.rs"]
mod terminal;
#[path = "unit/version.rs"]
mod version;
#[path = "unit/wire_dispatch.rs"]
mod wire_dispatch;
#[path = "unit/wire_fake.rs"]
mod wire_fake;
#[path = "unit/wire_tombstone.rs"]
mod wire_tombstone;
