use std::fmt;

use uuid::Uuid;

pub use crate::model_registry::CapabilityRequirements;

/// Strictness order is `LocalOnly` > `OnlineAllowed`. `OnlineRequired` is an
/// execution need, never a way to override an upstream `LocalOnly`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPolicy {
    LocalOnly,
    OnlineAllowed,
    OnlineRequired,
}

impl DataPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalOnly => "local_only",
            Self::OnlineAllowed => "online_allowed",
            Self::OnlineRequired => "online_required",
        }
    }

    pub fn allows_online(self) -> bool {
        !matches!(self, Self::LocalOnly)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputContract {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionRole {
    Decomposer,
    Worker,
    Integrator,
}

impl ExecutionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decomposer => "decomposer",
            Self::Worker => "worker",
            Self::Integrator => "integrator",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct TaskNode {
    pub id: String,
    pub objective: String,
    pub task_kind: String,
    pub dependencies: Vec<String>,
    pub required_capabilities: CapabilityRequirements,
    pub data_policy: DataPolicy,
    pub output_contract: OutputContract,
}

impl fmt::Debug for TaskNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskNode")
            .field("task_kind", &self.task_kind)
            .field("dependencies", &self.dependencies.len())
            .field("data_policy", &self.data_policy)
            .field("output_contract", &self.output_contract)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq)]
pub struct TaskGraph {
    pub orchestration_id: Uuid,
    pub nodes: Vec<TaskNode>,
}

impl fmt::Debug for TaskGraph {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskGraph")
            .field("orchestration_id", &self.orchestration_id)
            .field("nodes", &self.nodes.len())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DecompositionDecision {
    Direct { reason: String },
    Decomposed(TaskGraph),
}

impl DecompositionDecision {
    pub fn direct(reason: impl Into<String>) -> Self {
        Self::Direct {
            reason: reason.into(),
        }
    }
}
