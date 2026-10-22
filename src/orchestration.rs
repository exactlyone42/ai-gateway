pub mod contracts;
pub mod decomposer;
pub mod decomposition_eval;
pub mod graph;

pub use contracts::{
    CapabilityRequirements, DataPolicy, DecompositionDecision, ExecutionRole, OutputContract,
    TaskGraph, TaskNode,
};
pub use decomposition_eval::{DecompositionScore, Expectation, MIN_REQUIRED_RECALL};
pub use graph::{GraphValidationError, MAX_DEPTH, MAX_NODES, validate_graph};
