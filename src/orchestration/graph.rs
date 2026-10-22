use std::collections::HashMap;

use thiserror::Error;

use super::contracts::{DataPolicy, TaskGraph};

pub const MAX_NODES: usize = 8;
pub const MAX_DEPTH: usize = 2;

/// Variants carry no node IDs or text: the graph comes from model output and
/// must not leak into logs through an error message.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GraphValidationError {
    #[error("graph has no nodes")]
    Empty,
    #[error("graph has more than {MAX_NODES} nodes")]
    TooManyNodes,
    #[error("node id is empty")]
    EmptyId,
    #[error("duplicate node id")]
    DuplicateId,
    #[error("node objective is empty")]
    EmptyObjective,
    #[error("dependency refers to an unknown node")]
    UnknownDependency,
    #[error("dependency graph has a cycle")]
    Cycle,
    #[error("dependency chain is longer than {MAX_DEPTH} layers")]
    TooDeep,
    #[error("node data policy is looser than a dependency")]
    PolicyDowngrade,
}

impl GraphValidationError {
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::Empty => "graph_empty",
            Self::TooManyNodes => "graph_too_many_nodes",
            Self::EmptyId => "graph_empty_id",
            Self::DuplicateId => "graph_duplicate_id",
            Self::EmptyObjective => "graph_empty_objective",
            Self::UnknownDependency => "graph_unknown_dependency",
            Self::Cycle => "graph_cycle",
            Self::TooDeep => "graph_too_deep",
            Self::PolicyDowngrade => "graph_policy_downgrade",
        }
    }
}

pub fn validate_graph(graph: &TaskGraph) -> Result<(), GraphValidationError> {
    let nodes = &graph.nodes;
    if nodes.is_empty() {
        return Err(GraphValidationError::Empty);
    }
    if nodes.len() > MAX_NODES {
        return Err(GraphValidationError::TooManyNodes);
    }

    let mut index = HashMap::with_capacity(nodes.len());
    for (position, node) in nodes.iter().enumerate() {
        if node.id.trim().is_empty() {
            return Err(GraphValidationError::EmptyId);
        }
        if node.objective.trim().is_empty() {
            return Err(GraphValidationError::EmptyObjective);
        }
        if index.insert(node.id.as_str(), position).is_some() {
            return Err(GraphValidationError::DuplicateId);
        }
    }

    let mut parents = Vec::with_capacity(nodes.len());
    for node in nodes {
        let mut resolved = Vec::with_capacity(node.dependencies.len());
        for dependency in &node.dependencies {
            let position = *index
                .get(dependency.as_str())
                .ok_or(GraphValidationError::UnknownDependency)?;
            resolved.push(position);
        }
        parents.push(resolved);
    }

    // Kahn's algorithm: a node is placed once all its dependencies are placed,
    // so any node left over sits on or behind a cycle.
    let mut layer: Vec<Option<usize>> = vec![None; nodes.len()];
    let mut placed = 0;
    while placed < nodes.len() {
        let mut progressed = false;
        for position in 0..nodes.len() {
            if layer[position].is_some() {
                continue;
            }
            let deepest = parents[position]
                .iter()
                .map(|parent| layer[*parent])
                .try_fold(0, |deepest, parent| parent.map(|value| deepest.max(value)));
            if let Some(deepest) = deepest {
                layer[position] = Some(deepest + 1);
                placed += 1;
                progressed = true;
            }
        }
        if !progressed {
            return Err(GraphValidationError::Cycle);
        }
    }
    if layer.iter().flatten().any(|value| *value > MAX_DEPTH) {
        return Err(GraphValidationError::TooDeep);
    }

    for (position, node) in nodes.iter().enumerate() {
        let inherits_local = parents[position]
            .iter()
            .any(|parent| nodes[*parent].data_policy == DataPolicy::LocalOnly);
        if inherits_local && node.data_policy != DataPolicy::LocalOnly {
            return Err(GraphValidationError::PolicyDowngrade);
        }
    }
    Ok(())
}
