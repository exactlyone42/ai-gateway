use agent_pool_gateway::orchestration::{
    CapabilityRequirements, DataPolicy, DecompositionDecision, DecompositionScore, Expectation,
    OutputContract, TaskGraph, TaskNode,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    kind: String,
    required: Vec<String>,
    allowed: Vec<String>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/task_decomposition_cases.json")).unwrap()
}

fn node(kind: &str, position: usize) -> TaskNode {
    TaskNode {
        id: format!("t{position}"),
        objective: format!("objective {position}"),
        task_kind: kind.to_owned(),
        dependencies: Vec::new(),
        required_capabilities: CapabilityRequirements::default(),
        data_policy: DataPolicy::OnlineAllowed,
        output_contract: OutputContract::Text,
    }
}

fn graph_of(kinds: &[&str]) -> DecompositionDecision {
    DecompositionDecision::Decomposed(TaskGraph {
        orchestration_id: Uuid::from_u128(1),
        nodes: kinds
            .iter()
            .enumerate()
            .map(|(position, kind)| node(kind, position))
            .collect(),
    })
}

fn expectation(case: &Case) -> Expectation<'_> {
    if case.kind == "compound" {
        Expectation::Decomposed {
            required: &case.required,
            allowed: &case.allowed,
        }
    } else {
        Expectation::Direct
    }
}

/// The reference answer for every case: exact labels for compound cases and
/// `Direct` for the rest.
fn reference(case: &Case) -> DecompositionDecision {
    if case.kind == "compound" {
        let kinds: Vec<&str> = case.required.iter().map(String::as_str).collect();
        graph_of(&kinds)
    } else {
        DecompositionDecision::direct("reference")
    }
}

fn score_with(
    mut mutate: impl FnMut(&Case, DecompositionDecision) -> DecompositionDecision,
) -> DecompositionScore {
    let mut score = DecompositionScore::default();
    for case in &fixture().cases {
        score.record(&expectation(case), &mutate(case, reference(case)));
    }
    score
}

fn without_first_label(case: &Case) -> DecompositionDecision {
    let kinds: Vec<&str> = case.required.iter().skip(1).map(String::as_str).collect();
    graph_of(&kinds)
}

#[test]
fn corpus_has_enough_compound_and_simple_cases() {
    let fixture = fixture();
    let compound = fixture
        .cases
        .iter()
        .filter(|case| case.kind == "compound")
        .count();
    let other = fixture.cases.len() - compound;
    assert!(compound >= 30, "compound cases: {compound}");
    assert!(other >= 10, "simple and tool cases: {other}");
    let mut ids: Vec<&str> = fixture.cases.iter().map(|case| case.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), fixture.cases.len(), "duplicate case id");
    assert!(
        fixture
            .cases
            .iter()
            .filter(|case| case.kind == "compound")
            .all(|case| case.required.len() >= 2)
    );
}

#[test]
fn reference_answers_pass_the_gate() {
    let score = score_with(|_, decision| decision);
    assert!(score.meets_gate(), "{score:?}");
    assert_eq!(score.required_hit, score.required_total);
    assert_eq!(score.invented, 0);
    assert_eq!(score.direct_hit, score.direct_total);
}

#[test]
fn dropping_a_required_label_is_counted() {
    let mut dropped = false;
    let score = score_with(|case, decision| {
        if !dropped && case.kind == "compound" {
            dropped = true;
            return without_first_label(case);
        }
        decision
    });
    assert_eq!(score.required_hit + 1, score.required_total);
    assert!(score.required_recall() < 1.0);
}

#[test]
fn losing_more_than_a_tenth_of_labels_fails_the_gate() {
    let score = score_with(|case, decision| {
        if case.kind == "compound" {
            return without_first_label(case);
        }
        decision
    });
    assert!(!score.meets_gate(), "{score:?}");
}

#[test]
fn one_invented_label_fails_the_gate() {
    let mut added = false;
    let score = score_with(|case, decision| {
        if !added && case.kind == "compound" {
            added = true;
            let mut kinds: Vec<&str> = case.required.iter().map(String::as_str).collect();
            kinds.push("invented_kind");
            return graph_of(&kinds);
        }
        decision
    });
    assert_eq!(score.invented, 1);
    assert!(!score.meets_gate());
}

#[test]
fn splitting_a_simple_case_fails_the_gate() {
    let mut split = false;
    let score = score_with(|case, decision| {
        if !split && case.kind == "simple" {
            split = true;
            return graph_of(&["general_assistance", "research"]);
        }
        decision
    });
    assert_eq!(score.direct_hit + 1, score.direct_total);
    assert!(!score.meets_gate());
}

#[test]
fn answering_direct_for_a_compound_case_misses_its_labels() {
    let score = score_with(|_, decision| match decision {
        DecompositionDecision::Decomposed(_) => DecompositionDecision::direct("lazy"),
        direct => direct,
    });
    assert_eq!(score.required_hit, 0);
    assert!(!score.meets_gate());
}
