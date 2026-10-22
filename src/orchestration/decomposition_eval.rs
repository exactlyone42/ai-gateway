use std::collections::BTreeSet;

use super::contracts::DecompositionDecision;

pub const MIN_REQUIRED_RECALL: f64 = 0.9;

/// What a fixture case demands from the decomposer. Labels are `task_kind`
/// values; a node whose kind is in neither list counts as invented intent.
pub enum Expectation<'a> {
    Direct,
    Decomposed {
        required: &'a [String],
        allowed: &'a [String],
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecompositionScore {
    pub required_total: usize,
    pub required_hit: usize,
    pub invented: usize,
    pub direct_total: usize,
    pub direct_hit: usize,
}

impl DecompositionScore {
    pub fn record(&mut self, expectation: &Expectation<'_>, decision: &DecompositionDecision) {
        match expectation {
            Expectation::Direct => {
                self.direct_total += 1;
                self.direct_hit +=
                    usize::from(matches!(decision, DecompositionDecision::Direct { .. }));
            }
            Expectation::Decomposed { required, allowed } => {
                self.required_total += required.len();
                let DecompositionDecision::Decomposed(graph) = decision else {
                    return;
                };
                let produced: BTreeSet<&str> = graph
                    .nodes
                    .iter()
                    .map(|node| node.task_kind.as_str())
                    .collect();
                self.required_hit += required
                    .iter()
                    .filter(|label| produced.contains(label.as_str()))
                    .count();
                self.invented += produced
                    .iter()
                    .filter(|kind| {
                        !required
                            .iter()
                            .chain(allowed.iter())
                            .any(|label| label == *kind)
                    })
                    .count();
            }
        }
    }

    pub fn required_recall(&self) -> f64 {
        if self.required_total == 0 {
            return 1.0;
        }
        self.required_hit as f64 / self.required_total as f64
    }

    pub fn meets_gate(&self) -> bool {
        self.required_total > 0
            && self.direct_total > 0
            && self.required_recall() >= MIN_REQUIRED_RECALL
            && self.invented == 0
            && self.direct_hit == self.direct_total
    }
}
