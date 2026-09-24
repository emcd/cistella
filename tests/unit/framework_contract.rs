//! Framework contract unit tests: merge rules, baseline, deadlines.
//!
//! All cases exercise the public contract surface (`merge_prepare`,
//! `BaselineBinding::revalidate`, phase/deadline types). Mount
//! triples here are shape-valid only; topology rules stay covered by
//! the mount suite.

use cistella::framework::contract::{
    Assumption, BaselineBinding, Capability, CapabilitySet, ControlDeadline, Deadlines,
    EnvContribution, GuestHookRequest, MergeContext, MountContribution, MountMode, MountTriple,
    Phase, PolicyClaim, PreparePlan, Provenance, ReconciliationKey, Scope, Severity, merge_prepare,
};
use std::collections::{HashMap, HashSet};

fn full_capabilities() -> CapabilitySet {
    CapabilitySet::new(&[
        Capability::Environment,
        Capability::Mounts,
        Capability::PolicyClaims,
        Capability::GuestHooks,
    ])
}

fn env(name: &str, value: &str) -> EnvContribution {
    EnvContribution {
        name: name.to_string(),
        value: value.to_string(),
        provenance: Provenance::Extension("probe".to_string()),
    }
}

fn mount(target: &str) -> MountContribution {
    mount_from("/srv/data", target)
}

fn mount_from(host: &str, target: &str) -> MountContribution {
    MountContribution {
        triple: MountTriple {
            host_source: host.to_string(),
            container_target: target.to_string(),
            mode: MountMode::Ro,
        },
        provenance: Provenance::Extension("probe".to_string()),
    }
}

fn hook(order: u32) -> GuestHookRequest {
    GuestHookRequest {
        order,
        argv_prefix: vec!["/usr/libexec/landlock-wrap".to_string()],
        probe_op: "probe_capabilities".to_string(),
    }
}

fn claim(pattern: &str) -> PolicyClaim {
    PolicyClaim {
        pattern: pattern.to_string(),
        severity: Severity::Suppressible,
        scope: Scope::OnExtensions,
    }
}

#[test]
fn empty_plan_merges_without_capabilities() {
    let merged = merge_prepare(
        PreparePlan::default(),
        &CapabilitySet::default(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap();
    assert!(merged.environment.is_empty());
    assert!(merged.mounts.is_empty());
}

#[test]
fn undeclared_contribution_type_refuses_whole() {
    let plan = PreparePlan {
        environment: vec![env("PROBE_VAR", "1")],
        ..PreparePlan::default()
    };
    let error = merge_prepare(
        plan,
        &CapabilitySet::default(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("unadvertised"));
}

#[test]
fn duplicate_env_name_refuses_atomically() {
    let plan = PreparePlan {
        environment: vec![env("DUP_VAR", "1"), env("DUP_VAR", "2")],
        ..PreparePlan::default()
    };
    let error = merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("DUP_VAR"));
    assert!(!error.to_string().contains('1'));
}

#[test]
fn bad_env_name_and_control_value_refuse() {
    for contribution in [env("lowercase", "1"), env("BAD\nNAME", "1")] {
        let plan = PreparePlan {
            environment: vec![contribution],
            ..PreparePlan::default()
        };
        merge_prepare(
            plan,
            &full_capabilities(),
            &MergeContext::empty("/home/cistella"),
        )
        .unwrap_err();
    }
    let plan = PreparePlan {
        environment: vec![env("CTRL_VAR", "a\nb")],
        ..PreparePlan::default()
    };
    let error = merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("CTRL_VAR"));
}

#[test]
fn mount_topology_still_applies_to_contributions() {
    let plan = PreparePlan {
        mounts: vec![mount("/etc")],
        ..PreparePlan::default()
    };
    merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap_err();
}

#[test]
fn duplicate_hook_order_refuses_and_hooks_sort() {
    let plan = PreparePlan {
        guest_hooks: vec![hook(1), hook(1)],
        ..PreparePlan::default()
    };
    merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap_err();
    let plan = PreparePlan {
        guest_hooks: vec![hook(9), hook(3)],
        ..PreparePlan::default()
    };
    let merged = merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap();
    let orders: Vec<u32> = merged.guest_hooks.iter().map(|hook| hook.order).collect();
    assert_eq!(orders, vec![3, 9]);
}

#[test]
fn empty_hook_content_refuses() {
    let mut empty_argv = hook(1);
    empty_argv.argv_prefix.clear();
    let plan = PreparePlan {
        guest_hooks: vec![empty_argv],
        ..PreparePlan::default()
    };
    merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap_err();
    let mut empty_probe = hook(1);
    empty_probe.probe_op.clear();
    let plan = PreparePlan {
        guest_hooks: vec![empty_probe],
        ..PreparePlan::default()
    };
    merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap_err();
}

#[test]
fn empty_claim_pattern_refuses() {
    let plan = PreparePlan {
        policy_claims: vec![claim("")],
        ..PreparePlan::default()
    };
    merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap_err();
}

#[test]
fn reserved_env_names_refuse_atomically() {
    let reserved: HashSet<String> = ["HOME".to_string()].iter().cloned().collect();
    let occupied = Vec::new();
    let context = MergeContext::new("/home/cistella", reserved, occupied);
    let plan = PreparePlan {
        environment: vec![env("HOME", "/tmp/evil"), env("CLEAN_VAR", "1")],
        ..PreparePlan::default()
    };
    let error = merge_prepare(plan, &full_capabilities(), &context).unwrap_err();
    assert!(error.to_string().contains("HOME"));
}

#[test]
fn occupied_mount_targets_refuse_overlap() {
    let occupied = vec![MountTriple {
        host_source: "/srv/data".to_string(),
        container_target: "/data".to_string(),
        mode: MountMode::Ro,
    }];
    let context = MergeContext::new("/home/cistella", HashSet::new(), occupied);
    // Exact-target overlap with the emitted set refuses.
    let plan = PreparePlan {
        mounts: vec![mount("/data")],
        ..PreparePlan::default()
    };
    merge_prepare(plan, &full_capabilities(), &context).unwrap_err();
    // Disjoint targets merge, and only contributed triples return.
    let plan = PreparePlan {
        mounts: vec![mount_from("/srv/other", "/other")],
        ..PreparePlan::default()
    };
    let merged = merge_prepare(plan, &full_capabilities(), &context).unwrap();
    assert_eq!(merged.mounts.len(), 1);
    assert_eq!(merged.mounts[0].container_target, "/other");
}

#[test]
fn merged_plan_keeps_spine_order() {
    let plan = PreparePlan {
        environment: vec![env("FIRST_VAR", "1"), env("SECOND_VAR", "2")],
        mounts: vec![mount("/data")],
        policy_claims: vec![claim("FIRST_VAR")],
        guest_hooks: vec![hook(2)],
    };
    let merged = merge_prepare(
        plan,
        &full_capabilities(),
        &MergeContext::empty("/home/cistella"),
    )
    .unwrap();
    let names: Vec<&str> = merged
        .environment
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, vec!["FIRST_VAR", "SECOND_VAR"]);
    assert_eq!(merged.mounts.len(), 1);
    assert_eq!(merged.policy_claims.len(), 1);
    assert_eq!(merged.guest_hooks.len(), 1);
}

#[test]
fn baseline_revalidate_names_drift_without_state() {
    let baseline = BaselineBinding {
        profile_digest: "abc".to_string(),
        extensions: vec![],
        assumptions: vec![Assumption {
            key: "mount-source".to_string(),
            digest: "old".to_string(),
        }],
    };
    let mut live = HashMap::new();
    live.insert("mount-source".to_string(), "old".to_string());
    baseline.revalidate(&live).unwrap();
    live.insert("mount-source".to_string(), "new".to_string());
    let error = baseline.revalidate(&live).unwrap_err();
    assert!(error.to_string().contains("mount-source"));
    assert!(!error.to_string().contains("new"));
}

#[test]
fn phases_order_and_planning_purity() {
    let indices: Vec<u8> = [
        Phase::Planning,
        Phase::GateHostPreCreate,
        Phase::Create,
        Phase::Initiate,
        Phase::PostInitiatePrepare,
        Phase::GuestRestriction,
        Phase::ExecuteLaunch,
        Phase::AwaitResult,
        Phase::Teardown,
    ]
    .iter()
    .map(|phase| phase.index())
    .collect();
    let mut sorted = indices.clone();
    sorted.sort();
    assert_eq!(indices, sorted);
    assert!(Phase::Planning.is_planning());
    assert!(!Phase::Create.is_planning());
    assert!(!Phase::Teardown.is_planning());
}

#[test]
fn deadlines_cover_every_control_interaction() {
    let deadlines = Deadlines::default();
    for interaction in [
        ControlDeadline::Hello,
        ControlDeadline::Plan,
        ControlDeadline::Apply,
        ControlDeadline::TerminateGrace,
    ] {
        assert!(!deadlines.for_interaction(interaction).is_zero());
    }
}

#[test]
fn reconciliation_key_reuse_keeps_attempt_identity() {
    let first = ReconciliationKey::generate();
    let retry = ReconciliationKey::reuse(first.as_str());
    assert_eq!(first, retry);
    assert_ne!(first, ReconciliationKey::generate());
}
