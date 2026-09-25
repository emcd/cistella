## ADDED Requirements

### Requirement: External guest production path

Production `conduct` SHALL drive lifecycle through the external Podman guest binary over the stdio wire; the in-process isolator implementation SHALL remain as the conformance reference and fast-suite backend. Both paths SHALL share wire-schema types by construction, and any behavioral divergence SHALL surface as a conformance failure.

#### Scenario: Wire path carries production traffic
- **WHEN** conduct runs a session with the external guest present
- **THEN** every lifecycle operation crosses the framed protocol and the session converges identically to the reference path

#### Scenario: Divergence is conformance failure
- **WHEN** the wire guest and the in-process reference disagree on any lifecycle behavior
- **THEN** the conformance suite fails naming the divergent operation; the disagreement is never resolved by convention
