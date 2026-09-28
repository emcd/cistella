## ADDED Requirements

### Requirement: Live guest prepare and hook phases

The spine's prepare and guest-hook phases SHALL carry live traffic with real external guests: `prepare` returns actual env/mount contributions plus hook requests, the guest-context capability probe runs after initiate, apply confirms before execute, and wrapper diagnostics flow on the dedicated channel. Framework deadlines SHALL bound every exchange; timeouts fail pre-execute with no residue.

#### Scenario: Live prepare merges real contributions
- **WHEN** the Landlock extension returns its prepare transaction
- **THEN** central merge validates and admits the typed sets exactly as with the fixture peer, and the merged plan drives the session

#### Scenario: Probe failure fails live pre-exec
- **WHEN** the real guest's capability probe reports a required restriction unsupported
- **THEN** apply never runs, staged artifacts are removed, and conduct fails with a typed capability error
