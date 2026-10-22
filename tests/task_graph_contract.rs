use agent_pool_gateway::orchestration::{
    CapabilityRequirements, DataPolicy, GraphValidationError, OutputContract, TaskGraph, TaskNode,
    validate_graph,
};
use uuid::Uuid;

fn node(id: &str, dependencies: &[&str]) -> TaskNode {
    TaskNode {
        id: id.to_owned(),
        objective: format!("objective {id}"),
        task_kind: "analysis".to_owned(),
        dependencies: dependencies
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        required_capabilities: CapabilityRequirements::default(),
        data_policy: DataPolicy::OnlineAllowed,
        output_contract: OutputContract::Text,
    }
}

fn graph(nodes: Vec<TaskNode>) -> TaskGraph {
    TaskGraph {
        orchestration_id: Uuid::from_u128(1),
        nodes,
    }
}

#[test]
fn accepts_parallel_and_two_layer_graphs() {
    assert_eq!(
        validate_graph(&graph(vec![node("a", &[]), node("b", &[])])),
        Ok(())
    );
    assert_eq!(
        validate_graph(&graph(vec![node("a", &[]), node("b", &["a"])])),
        Ok(())
    );
    let full = (0..8)
        .map(|index| node(&format!("n{index}"), &[]))
        .collect();
    assert_eq!(validate_graph(&graph(full)), Ok(()));
}

#[test]
fn rejects_each_structural_violation_with_its_own_error() {
    let cases = [
        (graph(vec![]), GraphValidationError::Empty),
        (
            graph(vec![node("a", &[]), node("a", &[])]),
            GraphValidationError::DuplicateId,
        ),
        (
            graph(vec![node("a", &["missing"])]),
            GraphValidationError::UnknownDependency,
        ),
        (
            graph(vec![node("a", &["b"]), node("b", &["a"])]),
            GraphValidationError::Cycle,
        ),
        (graph(vec![node("a", &["a"])]), GraphValidationError::Cycle),
        (
            graph(
                (0..9)
                    .map(|index| node(&format!("n{index}"), &[]))
                    .collect(),
            ),
            GraphValidationError::TooManyNodes,
        ),
        (
            graph(vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])]),
            GraphValidationError::TooDeep,
        ),
        (graph(vec![node("", &[])]), GraphValidationError::EmptyId),
    ];
    for (input, expected) in cases {
        assert_eq!(validate_graph(&input), Err(expected));
    }

    let mut blank = node("a", &[]);
    blank.objective = "  ".to_owned();
    assert_eq!(
        validate_graph(&graph(vec![blank])),
        Err(GraphValidationError::EmptyObjective)
    );
}

#[test]
fn derived_nodes_cannot_loosen_a_local_only_dependency() {
    let mut local = node("a", &[]);
    local.data_policy = DataPolicy::LocalOnly;
    for loosened in [DataPolicy::OnlineAllowed, DataPolicy::OnlineRequired] {
        let mut child = node("b", &["a"]);
        child.data_policy = loosened;
        assert_eq!(
            validate_graph(&graph(vec![local.clone(), child])),
            Err(GraphValidationError::PolicyDowngrade)
        );
    }
    let mut child = node("b", &["a"]);
    child.data_policy = DataPolicy::LocalOnly;
    assert_eq!(validate_graph(&graph(vec![local, child])), Ok(()));
}

#[test]
fn errors_and_debug_output_do_not_echo_node_content() {
    let sentinel = "SENTINEL-secret-objective";
    let mut leaky = node("SENTINEL-id", &["SENTINEL-missing"]);
    leaky.objective = sentinel.to_owned();
    let input = graph(vec![leaky]);
    let error = validate_graph(&input).unwrap_err();
    let rendered = format!("{error} {error:?} {input:?}");
    assert!(!rendered.contains("SENTINEL"), "{rendered}");
}
