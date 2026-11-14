//! Host unit tests for the kernel's pure logic. The modules below are the exact
//! source files the bare-metal kernel compiles; nothing is copied.
extern crate alloc;

#[path = "../../hivemind-os/src/prov_core.rs"]
pub mod prov_core;
#[path = "../../hivemind-os/src/cap_core.rs"]
pub mod cap_core;
#[path = "../../hivemind-os/src/sched_core.rs"]
pub mod sched_core;

#[cfg(test)]
mod prov_tests {
    use crate::prov_core::*;

    fn emit(r: &mut Ring, kind: Kind, actor: Actor, parent: Option<u64>) -> u64 {
        r.emit(10, kind, actor, 1, "kernel-root", "temp", parent, value_attrs("95"))
    }

    #[test]
    fn ids_are_monotonic_and_parents_preserved() {
        let mut r = Ring::new(16);
        let a = emit(&mut r, Kind::Read, Actor::Agent(2), None);
        let b = emit(&mut r, Kind::Write, Actor::Agent(2), Some(a));
        assert_eq!((a, b), (1, 2));
        let e1 = r.pop().unwrap();
        let e2 = r.pop().unwrap();
        assert_eq!(e2.parent, Some(e1.id));
        assert!(r.pop().is_none());
    }

    #[test]
    fn full_ring_drops_new_events_and_counts_them() {
        let mut r = Ring::new(2);
        for _ in 0..5 { emit(&mut r, Kind::Write, Actor::Shell, None); }
        assert_eq!(r.emitted, 2);
        assert_eq!(r.dropped, 3);
        assert_eq!(r.next_id(), 6, "ids keep advancing so the collector sees the gap");
    }

    #[test]
    fn disabled_ring_records_nothing() {
        let mut r = Ring::new(4);
        r.enabled = false;
        assert_eq!(emit(&mut r, Kind::Write, Actor::Shell, None), 0);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn line_format_is_schema_v1_and_never_contains_the_value() {
        let mut r = Ring::new(4);
        r.emit(7, Kind::Write, Actor::Model(Some(3)), 4, "sh|ared", "k;ey", Some(9), value_attrs("SECRETVALUE"));
        let line = format_line(&r.pop().unwrap(), "uuid-1");
        assert_eq!(line, "PEV|1|uuid-1|1|7|write|model:agent:3|4|sh_ared|k_ey|9|len=11;vh=".to_string()
            + &alloc::format!("{:08x}", fnv32(b"SECRETVALUE")));
        assert!(!line.contains("SECRETVALUE"));
        assert_eq!(line.split('|').count(), 12);
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv32(b""), 0x811c9dc5);
        assert_eq!(fnv32(b"a"), 0xe40c292c);
    }

    #[test]
    fn trusted_principals() {
        assert!(Actor::Shell.is_trusted());
        assert!(Actor::Model(None).is_trusted());
        assert!(!Actor::Agent(1).is_trusted());
        assert!(!Actor::Model(Some(1)).is_trusted(), "model acting for an agent is checked as that agent");
        assert!(!Actor::Peer.is_trusted());
    }
}

#[cfg(test)]
mod cap_tests {
    use crate::cap_core::*;
    use alloc::string::ToString;

    fn table() -> CapTable { let mut t = CapTable::new(); t.set_enforce(true); t }

    #[test]
    fn not_enforced_allows_everything() {
        let t = CapTable::new();
        assert_eq!(t.check_node(Subject::Agent(1), OP_WRITE, 5, "x", 0), Decision::NotEnforced);
    }

    #[test]
    fn default_deny_when_enforced() {
        let t = table();
        assert!(matches!(t.check_node(Subject::Agent(1), OP_READ, 5, "x", 0),
            Decision::Deny { reason: DenyReason::NoCapability, .. }));
    }

    #[test]
    fn grant_covers_exact_node_and_op_only() {
        let mut t = table();
        let c = t.grant(Subject::Agent(1), Resource::Node(5), OP_READ, None, 0);
        assert!(matches!(t.check_node(Subject::Agent(1), OP_READ, 5, "x", 0), Decision::Allow { cap, .. } if cap == c));
        assert!(!t.check_node(Subject::Agent(1), OP_WRITE, 5, "x", 0).allowed(), "read cap must not allow write");
        assert!(!t.check_node(Subject::Agent(1), OP_READ, 6, "y", 0).allowed(), "other node");
        assert!(!t.check_node(Subject::Agent(2), OP_READ, 5, "x", 0).allowed(), "other agent (cross-agent access)");
    }

    #[test]
    fn prefix_namespace() {
        let mut t = table();
        t.grant(Subject::Agent(1), Resource::Prefix("research/".to_string()), OP_READ | OP_WRITE, None, 0);
        assert!(t.check_node(Subject::Agent(1), OP_WRITE, 9, "research/notes", 0).allowed());
        assert!(!t.check_node(Subject::Agent(1), OP_WRITE, 9, "secrets", 0).allowed());
    }

    #[test]
    fn expiry() {
        let mut t = table();
        t.grant(Subject::Agent(1), Resource::Node(5), OP_READ, Some(10), 100);
        assert!(t.check_node(Subject::Agent(1), OP_READ, 5, "x", 110).allowed());
        assert!(matches!(t.check_node(Subject::Agent(1), OP_READ, 5, "x", 111),
            Decision::Deny { reason: DenyReason::Expired, .. }));
    }

    #[test]
    fn revoke_cascades_to_delegations() {
        let mut t = table();
        let p = t.grant(Subject::Agent(1), Resource::Node(5), OP_READ | OP_DELEGATE, None, 0);
        let c = t.delegate(Subject::Agent(1), p, Subject::Agent(2), OP_READ, None, None, 0).unwrap();
        assert!(t.check_node(Subject::Agent(2), OP_READ, 5, "x", 0).allowed());
        assert_eq!(t.revoke(p), 2);
        assert!(matches!(t.check_node(Subject::Agent(2), OP_READ, 5, "x", 0),
            Decision::Deny { reason: DenyReason::Revoked, .. }));
        assert!(t.caps.iter().find(|x| x.id == c).unwrap().revoked);
    }

    #[test]
    fn delegation_cannot_exceed_authority() {
        let mut t = table();
        let p = t.grant(Subject::Agent(1), Resource::Prefix("research/".to_string()), OP_READ | OP_DELEGATE, Some(50), 0);
        assert_eq!(t.delegate(Subject::Agent(1), p, Subject::Agent(2), OP_WRITE, None, None, 0),
            Err(DelegateError::OpsExceedParent), "cannot delegate write when holding only read");
        assert_eq!(t.delegate(Subject::Agent(1), p, Subject::Agent(2), OP_READ,
            Some(Resource::Prefix("".to_string())), None, 0),
            Err(DelegateError::ResourceExceedsParent), "cannot widen namespace");
        assert_eq!(t.delegate(Subject::Agent(1), p, Subject::Agent(2), OP_READ, None, Some(100), 0),
            Err(DelegateError::ExpiryExceedsParent), "cannot outlive parent");
        let narrow = t.delegate(Subject::Agent(1), p, Subject::Agent(2), OP_READ,
            Some(Resource::Prefix("research/a".to_string())), None, 0);
        assert!(narrow.is_ok(), "narrower namespace is allowed");
        let child = t.caps.iter().find(|c| c.id == narrow.unwrap()).unwrap();
        assert_eq!(child.expires, Some(50), "inherits parent expiry");
    }

    #[test]
    fn only_holder_can_delegate_and_only_if_delegable() {
        let mut t = table();
        let p = t.grant(Subject::Agent(1), Resource::Node(5), OP_READ, None, 0);
        assert_eq!(t.delegate(Subject::Agent(9), p, Subject::Agent(9), OP_READ, None, None, 0),
            Err(DelegateError::NotHolder), "an agent cannot grant itself another agent's capability");
        assert_eq!(t.delegate(Subject::Agent(1), p, Subject::Agent(2), OP_READ, None, None, 0),
            Err(DelegateError::NotDelegable));
    }

    #[test]
    fn peer_is_a_separate_subject() {
        let mut t = table();
        t.grant(Subject::Agent(1), Resource::Prefix("".to_string()), OP_WRITE, None, 0);
        assert!(!t.check_node(Subject::Peer, OP_WRITE, 1, "kernel-root", 0).allowed());
        t.grant(Subject::Peer, Resource::Prefix("shared".to_string()), OP_WRITE, None, 0);
        assert!(t.check_node(Subject::Peer, OP_WRITE, 3, "shared", 0).allowed());
        assert!(!t.check_node(Subject::Peer, OP_WRITE, 1, "kernel-root", 0).allowed());
    }

    #[test]
    fn tool_invocation() {
        let mut t = table();
        assert!(!t.check_tool(Subject::Agent(1), "ai", 0).allowed());
        t.grant(Subject::Agent(1), Resource::Tool("ai".to_string()), OP_INVOKE, None, 0);
        assert!(t.check_tool(Subject::Agent(1), "ai", 0).allowed());
        assert!(!t.check_tool(Subject::Agent(1), "shell", 0).allowed());
    }

    #[test]
    fn policy_version_changes_with_policy() {
        let mut t = table();
        let v = t.version;
        let c = t.grant(Subject::Agent(1), Resource::Node(1), OP_READ, None, 0);
        assert!(t.version > v);
        let v2 = t.version;
        t.revoke(c);
        assert!(t.version > v2);
    }

    #[test]
    fn ops_parse_roundtrip() {
        assert_eq!(parse_ops("rwid"), Some(OP_READ | OP_WRITE | OP_INVOKE | OP_DELEGATE));
        assert_eq!(ops_string(OP_READ | OP_INVOKE), "r-i-");
        assert_eq!(parse_ops("rz"), None);
    }
}

#[cfg(test)]
mod sched_tests {
    use crate::sched_core::*;

    #[test]
    fn fifo_keeps_creation_order() {
        assert_eq!(order(Policy::Fifo, &[0, 5, 1]), alloc::vec![0, 1, 2]);
    }

    #[test]
    fn priority_orders_high_first_and_is_stable() {
        assert_eq!(order(Policy::Priority, &[1, 5, 1, 5]), alloc::vec![1, 3, 0, 2]);
    }

    #[test]
    fn budget_admits_in_order_and_defers_the_rest() {
        assert_eq!(admit(&[2, 3, 1], 0), alloc::vec![2, 3, 1]);
        assert_eq!(admit(&[2, 3, 1], 4), alloc::vec![2, 2, 0]);
    }
}
