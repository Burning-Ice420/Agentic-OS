//! Kernel scheduling hook: the order in which agents are evaluated each tick and
//! an optional per-tick budget of fired actions. Deliberately minimal; DAG,
//! deadline and model scheduling live in the user-space scheduler service.
//! Pure logic, unit-tested on the host by `hivemind-os-logic`.

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Creation order (the original behaviour).
    Fifo,
    /// Higher priority first; ties keep creation order.
    Priority,
}

impl Policy {
    pub fn name(&self) -> &'static str {
        match self { Policy::Fifo => "fifo", Policy::Priority => "priority" }
    }
}

pub struct SchedConfig {
    pub policy: Policy,
    /// Max fired actions applied per tick (0 = unlimited). Excess is deferred.
    pub budget: usize,
}

impl SchedConfig {
    pub const fn new() -> Self {
        SchedConfig { policy: Policy::Fifo, budget: 0 }
    }
}

/// Indices into the agent list in evaluation order. `prios[i]` is agent i's priority.
pub fn order(policy: Policy, prios: &[i32]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..prios.len()).collect();
    if policy == Policy::Priority {
        // stable sort keeps creation order among equal priorities
        idx.sort_by(|a, b| prios[*b].cmp(&prios[*a]));
    }
    idx
}

/// Given the number of actions each agent wants to fire this tick (in evaluation
/// order), return how many of each are applied now under `budget` (0 = unlimited).
pub fn admit(wanted: &[usize], budget: usize) -> Vec<usize> {
    if budget == 0 {
        return wanted.to_vec();
    }
    let mut left = budget;
    wanted.iter().map(|w| {
        let take = (*w).min(left);
        left -= take;
        take
    }).collect()
}

/// Running statistics, reported by `sched stats`.
#[derive(Default)]
pub struct SchedStats {
    pub ticks: u64,
    pub fired: u64,
    pub deferred: u64,
    pub max_wait_ticks: u64,
}
