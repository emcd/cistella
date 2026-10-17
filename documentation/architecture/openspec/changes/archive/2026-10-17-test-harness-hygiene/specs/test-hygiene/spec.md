## ADDED Requirements

### Requirement: Harness-only changes carry no normative behavior change

Test-harness and structural-split work under this change SHALL
alter no specified product behavior. Every step SHALL keep the
full fast suite green on both umasks; splits move code without
logic edits. This delta exists to satisfy the change-schema
gate explicitly (Tier-1 process observation): its sole normative
content is the proof obligation itself.

#### Scenario: Green suite per step
- **WHEN** any harness bound, fixture, tag, diagnostic, or split
  lands
- **THEN** the fast suite passes on both umasks before the next
  step begins
