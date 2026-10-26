//! Unit tests entry point.
//!
//! Each submodule pulls in a `.rs` file from `tests/unit/` and is
//! compiled into this single test binary. Cargo's autotests discovery
//! picks up `tests/unit.rs`; the `#[path]` attribute directs each
//! submodule to its file in the subdirectory.

#[path = "unit/attestation.rs"]
mod attestation;
#[path = "unit/cleanup.rs"]
mod cleanup;
#[path = "unit/cli_args.rs"]
mod cli_args;
#[path = "unit/compile_binding.rs"]
mod compile_binding;
#[path = "unit/conduct_eval.rs"]
mod conduct_eval;
#[path = "unit/credentials.rs"]
mod credentials;
#[path = "unit/describe.rs"]
mod describe;
#[path = "unit/discovery.rs"]
mod discovery;
#[path = "unit/environment_acceptances.rs"]
mod environment_acceptances;
#[path = "unit/fdpass.rs"]
mod fdpass;
#[path = "unit/framework_contract.rs"]
mod framework_contract;
#[path = "unit/guest_composition.rs"]
mod guest_composition;
#[path = "unit/guest_hosting.rs"]
mod guest_hosting;
#[path = "unit/guest_registry.rs"]
mod guest_registry;
#[path = "unit/guest_resolution.rs"]
mod guest_resolution;
#[path = "unit/guest_source.rs"]
mod guest_source;
#[path = "unit/identity.rs"]
mod identity;
#[path = "unit/isolator_trait.rs"]
mod isolator_trait;
#[path = "unit/lock_teardown.rs"]
mod lock_teardown;
#[path = "unit/mount.rs"]
mod mount;
#[path = "unit/mount_alias.rs"]
mod mount_alias;
#[path = "unit/mount_init.rs"]
mod mount_init;
#[path = "unit/mount_permissions.rs"]
mod mount_permissions;
#[path = "unit/operation_log.rs"]
mod operation_log;
#[path = "unit/operation_log_gc.rs"]
mod operation_log_gc;
#[path = "unit/operation_log_prune.rs"]
mod operation_log_prune;
#[path = "unit/operation_log_reader.rs"]
mod operation_log_reader;
#[path = "unit/policy_prepare.rs"]
mod policy_prepare;
#[path = "unit/prepare.rs"]
mod prepare;
#[path = "unit/prepare_hooks.rs"]
mod prepare_hooks;
#[path = "unit/prepare_lease.rs"]
mod prepare_lease;
#[path = "unit/prepare_permissions.rs"]
mod prepare_permissions;
#[path = "unit/profile_resolution.rs"]
mod profile_resolution;
#[path = "unit/protocol.rs"]
mod protocol;
#[path = "unit/quadlet_gc_classify.rs"]
mod quadlet_gc_classify;
#[path = "unit/quadlet_remove.rs"]
mod quadlet_remove;
#[path = "unit/registry.rs"]
mod registry;
#[path = "unit/runtime.rs"]
mod runtime;
#[path = "unit/ssh_ambient.rs"]
mod ssh_ambient;
#[path = "unit/stream.rs"]
mod stream;
#[path = "unit/terminal.rs"]
mod terminal;
#[path = "unit/unobservable.rs"]
mod unobservable;
#[path = "unit/version.rs"]
mod version;
#[path = "unit/wire_dispatch.rs"]
mod wire_dispatch;
#[path = "unit/wire_fake.rs"]
mod wire_fake;
#[path = "unit/wire_tombstone.rs"]
mod wire_tombstone;
