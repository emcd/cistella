# extension-protocol Specification

## Purpose
External guest protocol (0.2.0): synced from earlier changes. Update Purpose after archive.
## MODIFIED Requirements
### Requirement: Prepare transaction with central merge

Extensions SHALL return at most one prepare transaction per session with separately-typed environment sets, mount sets, policy claims, and guest-hook requests. The prepare request SHALL carry the opaque session id minted for the session (containers, scratch, and extension state share one identity); extensions SHALL treat it as opaque (never parse it) and MAY use it to mint per-session host state. The framework SHALL validate each set with its existing rules, merge centrally in spine order, and evaluate policy claims against the severity × scope lattice (tighten-only downward across site > user > defaults). Extension mount contributions SHALL merge through the central validators like every other set — including read-write triples (operator direction: declarations are authoritative intent; the framework enforces them, it does not second-guess them). This spec owns claim expression over stdio; lattice semantics live in the framework-lifecycle spec. Extension output is untrusted input at every step.

#### Scenario: Policy claim tightens only
- **WHEN** an extension claims a constraint weaker than the applicable site/user rule with otherwise valid shape
- **THEN** the framework discards the weakening and enforces the stricter rule with a typed diagnostic; the transaction proceeds

#### Scenario: Invalid policy claim refuses whole
- **WHEN** a `policy_claim` is malformed, overreaches its transaction, or carries unknown fields
- **THEN** the framework refuses the entire prepare transaction with a typed error; "typed error but continue" is not an outcome (atomicity holds)

#### Scenario: Guest-hook requests are explicit
- **WHEN** an extension requests guest pre-exec presence (e.g. a Landlock wrapper as exec ancestor)
- **THEN** the framework records the hook with its dedicated diagnostics channel; hooks never emit on protocol stdout nor hijack harness stdio

#### Scenario: Prepare carries the session id
- **WHEN** an extension answers prepare
- **THEN** the request names the opaque session id; the guest MAY mint per-session host state keyed by it and MUST NOT parse it

#### Scenario: Read-write mount contribution merges
- **WHEN** an extension contributes a read-write triple with otherwise valid shape (topology validates, no collision, lattice evaluates)
- **THEN** the merged plan carries it like any declared triple; no blanket read-write refusal exists
