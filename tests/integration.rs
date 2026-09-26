//! Integration tests entry point.

#[path = "integration/conformance.rs"]
mod conformance;
#[path = "integration/design_vectors.rs"]
mod design_vectors;
#[path = "integration/guest_hosting.rs"]
mod guest_hosting;
#[path = "integration/helpers.rs"]
mod helpers;
#[path = "integration/labels.rs"]
mod labels;
#[path = "integration/landlock_spike.rs"]
mod landlock_spike;
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
#[path = "integration/recovery.rs"]
mod recovery;
#[path = "integration/selectors.rs"]
mod selectors;
#[path = "integration/signals.rs"]
mod signals;
#[path = "integration/transport.rs"]
mod transport;
#[path = "integration/wire_death.rs"]
mod wire_death;
#[path = "integration/wire_parity.rs"]
mod wire_parity;
