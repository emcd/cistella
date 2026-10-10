# guest-hosting Specification

## Purpose
External guest hosting (0.2.0): synced from change external-guests-src-confinement. Update Purpose after archive.
## Requirements
## MODIFIED Requirements
### Requirement: Profile-declared guest selection

Conduct SHALL host exactly the isolator and extensions a profile declares (`[isolator]` table, `[[extensions]]` array of tables): the named isolator backend conducts the session, and each named extension answers prepare. Absent tables mean pure Podman behavior (2.x path — no extension guest spawned, no hooks staged; the Podman isolator guest itself is still hosted as in 2.x, and 0.1.x profiles run unchanged). Names SHALL resolve first as shipped guests (`podman`, `landlock`, sibling-relative discovery as today), then through the seat-scoped guest registry; names with neither a shipped implementation nor a registry entry, and duplicate extension entries, SHALL refuse pre-create with a typed error naming the declaration (registration is opt-in per guest — an unregistered name can never silently plain a meant-hooked session). At most one extension per profile conducts in v1 (extension composition — multi-extension merge order and conflict semantics — is a later design; two declared extensions refuse pre-create rather than merge unspecified).

#### Scenario: Undeclared sessions run plain
- **WHEN** a profile declares no extensions
- **THEN** conduct spawns no extension guest and runs the plain lifecycle (the isolator guest is hosted as usual); no prepare transaction occurs

#### Scenario: Declared Landlock engages hooks
- **WHEN** a profile declares the `landlock` extension
- **THEN** the Landlock guest answers prepare and its hook stages before create, and the session conducts on the hooked path

#### Scenario: Selected-but-empty hooks refuse pre-create
- **WHEN** a profile declares the `landlock` extension but the guest answers zero hooks (malformed, buggy, or replaced guest)
- **THEN** conduct refuses pre-create with a typed error (never a silent plain session); the selection guarantees engagement

#### Scenario: Unknown guest refuses pre-create
- **WHEN** a profile names an isolator or extension with no shipped implementation and no registry entry
- **THEN** conduct refuses pre-create with a typed error naming the declaration; no session starts undeclared-unhooked

#### Scenario: Registered third-party guest resolves
- **WHEN** a profile names a guest with a valid registry entry (verified source, matching role)
- **THEN** conduct hosts it exactly as a shipped guest of that role (prepare for extensions, lifecycle for isolators); verification failures refuse pre-create typed
