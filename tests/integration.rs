//! Integration tests entry point.

#[path = "integration/bounded.rs"]
mod bounded;
#[path = "integration/conduct_taps.rs"]
mod conduct_taps;
#[path = "integration/conformance.rs"]
mod conformance;
#[path = "integration/describe_gate.rs"]
mod describe_gate;
#[path = "integration/design_vectors.rs"]
mod design_vectors;
#[path = "integration/enter_gate.rs"]
mod enter_gate;
#[path = "integration/event_wait.rs"]
mod event_wait;
#[path = "integration/gates.rs"]
mod gates;
#[path = "integration/guest_hello.rs"]
mod guest_hello;
#[path = "integration/guest_hosting.rs"]
mod guest_hosting;
#[path = "integration/harness_bounds.rs"]
mod harness_bounds;
#[path = "integration/helpers.rs"]
mod helpers;
#[path = "integration/hook_live.rs"]
mod hook_live;
#[path = "integration/labels.rs"]
mod labels;
#[path = "integration/landlock_spike.rs"]
mod landlock_spike;
#[path = "integration/landlock_wrapper.rs"]
mod landlock_wrapper;
#[path = "integration/lifecycle.rs"]
mod lifecycle;
#[path = "integration/mountprep.rs"]
mod mountprep;
#[path = "integration/orphan.rs"]
mod orphan;
#[path = "integration/podman_ancestry.rs"]
mod podman_ancestry;
#[path = "integration/prepare_peer.rs"]
mod prepare_peer;
#[path = "integration/protocol_peer.rs"]
mod protocol_peer;
#[path = "integration/pty.rs"]
mod pty;
#[path = "integration/recovery.rs"]
mod recovery;
#[path = "integration/selectors.rs"]
mod selectors;
#[path = "integration/signals.rs"]
mod signals;
#[path = "integration/ssh_guest.rs"]
mod ssh_guest;
#[path = "integration/teardown_reload.rs"]
mod teardown_reload;
#[path = "integration/terminate_converge.rs"]
mod terminate_converge;
#[path = "integration/tmp_alias.rs"]
mod tmp_alias;
#[path = "integration/tmpdir_gc.rs"]
mod tmpdir_gc;
#[path = "integration/transport.rs"]
mod transport;
#[path = "integration/wire_death.rs"]
mod wire_death;
#[path = "integration/wire_parity.rs"]
mod wire_parity;
