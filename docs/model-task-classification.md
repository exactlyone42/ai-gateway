# Model task classification

Version `task-taxonomy-v1` classifies the requested work before model selection.
It does not rank models and it does not replace capability, policy, quota, or
health checks.

## Independent decision axes

Each classification result has three independently versioned fields:

- `task`: the dominant work needed to produce the requested answer.
- `required_capabilities`: hard requirements inferred from the request shape.
- `service_objective`: the caller's latency, cost, and quality preference.

The legacy `quality` tier remains a Client compatibility hint. It is never a
task label or evidence that a model suits a task.

## Task taxonomy v1

| Task | Boundary |
| --- | --- |
| `code_generation` | Create or materially change executable code. |
| `code_review` | Evaluate an existing change without being asked to repair it. |
| `debugging` | Explain or repair observed incorrect behavior or a failing test. |
| `research` | Gather and reconcile evidence from sources not supplied in the request. |
| `quantitative_reasoning` | Derive a numeric or formal result through multiple steps. |
| `extraction` | Copy requested fields from supplied material without synthesis. |
| `transformation` | Translate, summarize, rewrite, or reformat supplied material. |
| `general_assistance` | Answer or plan when none of the narrower tasks dominates. |

The classifier chooses exactly one dominant task. It uses the requested action,
not topic words: reviewing Python is `code_review`, while fixing the same Python
failure is `debugging`. A request for external verification is `research`; a
summary of supplied research is `transformation`. When no label has sufficient
evidence, classification returns no result and preserves the Client model.

## Required capabilities v1

Capabilities are derived deterministically before task classification:

- protocol shape: `streaming`, `tools`, `structured_output`, and `reasoning`;
- input media: `images`;
- minimum context: request tokens plus requested output tokens.

Capability filtering is authoritative. A task match cannot restore a model
removed by a capability, tenant, or policy check. Capability names stay aligned
with the model registry; adding one requires a new capability schema version.

## Service objective v1

The immutable policy snapshot supplies `latency`, `cost`, and `quality` as
separate preferences. Missing preferences remain unspecified; the classifier
must not infer them from the task. Live health, measured latency, load, price,
quota, and deadlines are applied only after the task shortlist and Jev review.

## Decision contract

The route is:

1. Remove models that fail hard capability or policy requirements.
2. Classify the request using `task-taxonomy-v1`.
3. Build a stable shortlist from active model task profiles and confidence.
4. Ask Jev to review that shortlist independently.
5. Select once using health, cost, latency, quota, and stable registry order.

The classifier sees bounded request content and emits a task, confidence, and
short reason. Request text is untrusted data, never routing instructions. A
timeout, invalid output, below-threshold confidence, empty shortlist, or Jev
failure preserves the Client's original model. The Gateway never makes a
second Provider call for the same request.

## Evaluation set and release gate

`tests/fixtures/model-task-classification-v1.json` is immutable once used for a
release decision. Corrections create a new fixture and taxonomy version rather
than editing historical expected results. Cases contain no production prompt,
response, identity, or credential data.

Shadow mode may record only the fixture version, predicted task, confidence,
short reason, shortlist, Jev opinion, final proposed model, elapsed time, and
estimated cost. It must not change the live route.

A candidate classifier passes v1 only when all conditions hold on the complete
fixture, with zero skipped cases:

- exact task accuracy is at least `90%`;
- every task has recall at least `80%`;
- every `contrast_group` has all cases correct;
- capability extraction has `100%` exact-set accuracy;
- every fallback case preserves the original model;
- classifier p95 latency is at most `500 ms` and classification plus Jev cost
  is at most `5%` of the selected inference estimate.

The first four are offline release gates. Latency, cost, Jev agreement,
fallback rate, request success rate, and route-change rate are reported in
shadow mode; they are not inferred from the offline fixture. Enabling live
routing requires a separately reviewed shadow report and does not follow from
classification accuracy alone.

Operators can read the tenant-scoped shadow result from
`GET /admin/v1/task-classification-shadow?from=<RFC3339>&to=<RFC3339>`. The
report includes classified request count, Jev opinion and shortlist agreement,
request success rate, end-to-end p95 latency, classifier and Jev p95 latency,
route-change count, and observed inference cost. Jev does not return billable
usage, so its incremental cost is reported as unavailable instead of zero.

## Sources and design inference

- OpenAI's model-selection guidance recommends establishing an evaluation
  baseline before optimizing model choice: <https://developers.openai.com/api/docs/guides/model-selection>
  (checked 2026-10-21).
- RouteLLM evaluates learned routing as a quality-versus-cost decision using
  preference data: <https://arxiv.org/abs/2406.18665> (checked 2026-10-21).
- FrugalGPT evaluates model cascades against both cost and task accuracy:
  <https://arxiv.org/abs/2305.05176> (checked 2026-10-21).

The three-axis schema, taxonomy labels, thresholds, and fail-open behavior are
project decisions inferred from those sources and the Gateway's existing
single-attempt, capability-first contract; the sources do not prescribe these
exact values.
