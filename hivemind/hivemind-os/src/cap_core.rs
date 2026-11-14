//! Capability core: least-privilege access control for agents, the model acting
//! on an agent's behalf, and mesh peers. Pure logic (only `core` + `alloc`), so
//! it is unit-tested on the host by `hivemind-os-logic`.
//!
//! Semantics (also in research-docs/THREAT_MODEL.md):
//! * Enforcement is OFF by default (backward compatible). `enforce = true` makes
//!   every check default-deny: an operation is allowed only if a live capability
//!   for that subject covers the resource and the operation.
//! * Only the admin (the shell) issues capabilities. A subject can delegate a
//!   capability it holds only if that capability carries DELEGATE, and the
//!   delegated ops/resource/expiry can never exceed the parent's.
//! * Revoking a capability revokes every capability delegated from it.
//! * Every decision is explainable: it names the capability that matched, or the
//!   reason for denial, and the policy version it was taken under.

use alloc::string::String;
use alloc::vec::Vec;

pub const OP_READ: u8 = 1;
pub const OP_WRITE: u8 = 2;
pub const OP_INVOKE: u8 = 4;
pub const OP_DELEGATE: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    Agent(u64),
    Peer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// One memory node by id.
    Node(u64),
    /// Every memory node whose name starts with this prefix ("" = all nodes).
    Prefix(String),
    /// A tool such as the model accelerator ("ai").
    Tool(String),
}

impl Resource {
    fn covers(&self, other: &Resource) -> bool {
        match (self, other) {
            (Resource::Node(a), Resource::Node(b)) => a == b,
            (Resource::Prefix(p), Resource::Prefix(q)) => q.starts_with(p.as_str()),
            (Resource::Tool(a), Resource::Tool(b)) => a == b,
            _ => false,
        }
    }

    fn matches_node(&self, id: u64, name: &str) -> bool {
        match self {
            Resource::Node(n) => *n == id,
            Resource::Prefix(p) => name.starts_with(p.as_str()),
            Resource::Tool(_) => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issuer {
    Admin,
    Delegated { from_cap: u64 },
}

#[derive(Clone, Debug)]
pub struct Cap {
    pub id: u64,
    pub subject: Subject,
    pub resource: Resource,
    pub ops: u8,
    /// Tick after which the capability is dead (None = no expiry).
    pub expires: Option<u64>,
    pub issuer: Issuer,
    pub revoked: bool,
}

impl Cap {
    fn live(&self, now: u64) -> bool {
        !self.revoked && self.expires.map_or(true, |e| now <= e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    NoCapability,
    Expired,
    Revoked,
}

impl DenyReason {
    pub fn name(&self) -> &'static str {
        match self {
            DenyReason::NoCapability => "no_capability",
            DenyReason::Expired => "expired",
            DenyReason::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    NotEnforced,
    Allow { cap: u64, policy_version: u64 },
    Deny { reason: DenyReason, policy_version: u64 },
}

impl Decision {
    pub fn allowed(&self) -> bool {
        !matches!(self, Decision::Deny { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelegateError {
    NoSuchCap,
    NotHolder,
    NotDelegable,
    OpsExceedParent,
    ResourceExceedsParent,
    ExpiryExceedsParent,
    ParentDead,
}

pub struct CapTable {
    pub caps: Vec<Cap>,
    next_id: u64,
    pub enforce: bool,
    /// Incremented on every change, recorded with every decision.
    pub version: u64,
}

impl CapTable {
    pub const fn new() -> Self {
        CapTable { caps: Vec::new(), next_id: 1, enforce: false, version: 1 }
    }

    pub fn set_enforce(&mut self, on: bool) {
        self.enforce = on;
        self.version += 1;
    }

    /// Admin grant (only the shell calls this).
    pub fn grant(&mut self, subject: Subject, resource: Resource, ops: u8, ttl: Option<u64>, now: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.caps.push(Cap {
            id, subject, resource, ops,
            expires: ttl.map(|t| now.saturating_add(t)),
            issuer: Issuer::Admin,
            revoked: false,
        });
        self.version += 1;
        id
    }

    /// `holder` delegates part of capability `cap_id` to `to`. Can never exceed the parent.
    pub fn delegate(&mut self, holder: Subject, cap_id: u64, to: Subject, ops: u8,
                    resource: Option<Resource>, ttl: Option<u64>, now: u64) -> Result<u64, DelegateError> {
        let parent = self.caps.iter().find(|c| c.id == cap_id).cloned().ok_or(DelegateError::NoSuchCap)?;
        if parent.subject != holder {
            return Err(DelegateError::NotHolder);
        }
        if !parent.live(now) {
            return Err(DelegateError::ParentDead);
        }
        if parent.ops & OP_DELEGATE == 0 {
            return Err(DelegateError::NotDelegable);
        }
        if ops & !parent.ops != 0 {
            return Err(DelegateError::OpsExceedParent);
        }
        let res = resource.unwrap_or_else(|| parent.resource.clone());
        if !parent.resource.covers(&res) {
            return Err(DelegateError::ResourceExceedsParent);
        }
        let expires = match (ttl.map(|t| now.saturating_add(t)), parent.expires) {
            (Some(c), Some(p)) if c > p => return Err(DelegateError::ExpiryExceedsParent),
            (None, Some(p)) => Some(p),
            (c, _) => c,
        };
        let id = self.next_id;
        self.next_id += 1;
        self.caps.push(Cap { id, subject: to, resource: res, ops, expires,
                             issuer: Issuer::Delegated { from_cap: cap_id }, revoked: false });
        self.version += 1;
        Ok(id)
    }

    /// Revoke a capability and, transitively, everything delegated from it.
    /// Returns the number of capabilities newly revoked.
    pub fn revoke(&mut self, cap_id: u64) -> usize {
        let mut frontier: Vec<u64> = alloc::vec![cap_id];
        let mut count = 0;
        while let Some(id) = frontier.pop() {
            for c in self.caps.iter_mut() {
                if c.id == id && !c.revoked {
                    c.revoked = true;
                    count += 1;
                }
                if let Issuer::Delegated { from_cap } = c.issuer {
                    if from_cap == id && !c.revoked {
                        frontier.push(c.id);
                    }
                }
            }
        }
        if count > 0 {
            self.version += 1;
        }
        count
    }

    fn decide<F: Fn(&Resource) -> bool>(&self, subject: Subject, op: u8, now: u64, res_ok: F) -> Decision {
        if !self.enforce {
            return Decision::NotEnforced;
        }
        let mut saw_expired = false;
        let mut saw_revoked = false;
        for c in &self.caps {
            if c.subject != subject || c.ops & op == 0 || !res_ok(&c.resource) {
                continue;
            }
            if c.live(now) {
                return Decision::Allow { cap: c.id, policy_version: self.version };
            }
            if c.revoked { saw_revoked = true; } else { saw_expired = true; }
        }
        let reason = if saw_revoked {
            DenyReason::Revoked
        } else if saw_expired {
            DenyReason::Expired
        } else {
            DenyReason::NoCapability
        };
        Decision::Deny { reason, policy_version: self.version }
    }

    pub fn check_node(&self, subject: Subject, op: u8, mem_id: u64, mem_name: &str, now: u64) -> Decision {
        self.decide(subject, op, now, |r| r.matches_node(mem_id, mem_name))
    }

    pub fn check_tool(&self, subject: Subject, tool: &str, now: u64) -> Decision {
        self.decide(subject, OP_INVOKE, now, |r| matches!(r, Resource::Tool(t) if t == tool))
    }
}

/// Parse an ops string like "rw", "rwid" into a bitmask.
pub fn parse_ops(s: &str) -> Option<u8> {
    let mut m = 0u8;
    for c in s.chars() {
        m |= match c {
            'r' => OP_READ,
            'w' => OP_WRITE,
            'i' | 'x' => OP_INVOKE,
            'd' => OP_DELEGATE,
            '-' => 0,
            _ => return None,
        };
    }
    Some(m)
}

pub fn ops_string(ops: u8) -> String {
    let mut s = String::new();
    for (bit, c) in [(OP_READ, 'r'), (OP_WRITE, 'w'), (OP_INVOKE, 'i'), (OP_DELEGATE, 'd')] {
        s.push(if ops & bit != 0 { c } else { '-' });
    }
    s
}
