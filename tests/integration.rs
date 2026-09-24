//! Integration tests entry point.

#[path = "integration/conformance.rs"]
mod conformance;
#[path = "integration/design_vectors.rs"]
mod design_vectors;
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
#[path = "integration/prepare_peer.rs"]
mod prepare_peer;
#[path = "integration/protocol_peer.rs"]
mod protocol_peer;
#[path = "integration/selectors.rs"]
mod selectors;
#[path = "integration/signals.rs"]
mod signals;
#[path = "integration/transport.rs"]
mod transport;
