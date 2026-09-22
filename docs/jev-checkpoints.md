# Jev decision checkpoints

Consider Jev at every meaningful decision point. Apply explicit instructions and deterministic evidence first; call Jev only for a bounded judgment that remains. Consideration is a guidance obligation, not an API call per action. The harness observes submitted checkpoints, not private reasoning or every interactive tool call.

## Where to consider it

- `tool_selection`: choose the next permitted tool or read-only investigation step.
- `reasoning_allocation`: assess whether a narrow judgment or deeper investigation is needed.
- `identification`: classify supplied task intent, issue category, subsystem, or document type.
- `prioritization`: rank candidates using an explicit impact rubric; do not infer priority from emphatic wording.
- `evidence_selection`: choose a supplied source or observation that would resolve uncertainty.
- `evidence_assessment`: flag missing evidence, contradictions, or unsupported claims.
- `progress_assessment`: recognize repeated approaches and suggest a different investigation step.
- `handoff_assessment`: check whether a handoff addresses supplied requirements.
- `context_selection`: choose supplied context relevant to the next step or handoff.
- `clarification_assessment`: distinguish discoverable missing facts from user-owned preferences.

Classifications, evidence judgments, and completion assessments are hypotheses. Check them against authoritative sources. Jev cannot authorize actions, waive tests, dismiss failures, override a user choice, or declare work complete. Model/effort recommendations do not change the running agent automatically.

## Workflow

1. Apply rules locally. When they settle the decision, act according to them without a Jev call. For a recorded checkpoint use `bypass_reason`: `explicit_rule`, `user_choice`, `required_check`, `known_failure`, `authorization`, `irreversible`, `unchanged_state`, or `not_bounded`. An empty questions map is allowed for bypasses.
2. For an eligible judgment, record the intended action before requesting Jev. Use only a concise redacted goal, facts, constraints, and risks. No raw prompts, code, diffs, credentials, or personal data. Include an unknown/other choice when needed.
3. Batch independent questions against the same context. Dependent questions require fresh evidence. Do not ask again without a meaningful change to evidence, options, or constraints.
4. Run the checkpoint in shadow mode. Follow the pre-recorded baseline action within existing authorization. If new authoritative evidence changes the action, record that change; do not treat Jev's answer alone as new evidence.
5. After the decision resolves, record the action actually taken and an independently supported outcome. Agreement or accepting a suggestion does not establish correctness. Use `unknown` while it cannot be determined; leave the checkpoint unlabeled if evidence may arrive later.

## Version 2 input

`scripts/harness advise --context checkpoint.json` accepts this format alongside the unchanged v1 input:

```json
{
  "version": 2,
  "checkpoint": {
    "family": "tool_selection",
    "question_version": "next-evidence-1",
    "policy_version": "shadow-1",
    "baseline_action": "inspect",
    "bypass_reason": "none",
    "state_build_ms": null
  },
  "context": {
    "goal": "Choose the next read-only step to locate a missing handler.",
    "facts": ["The graph result is stale; the current source is available."],
    "constraints": ["Existing graph coverage rules remain mandatory."]
  },
  "questions": {
    "recommendation": {
      "type": "choice",
      "instructions": "Which next step is most likely to locate the handler?",
      "criteria": {"inspect": "Read the current candidate source", "trace": "Trace a related caller"}
    },
    "sufficient": {
      "type": "boolean",
      "instructions": "The supplied evidence establishes the handler location."
    },
    "impact": {
      "type": "score",
      "instructions": "How much investigation would a wrong next step waste?",
      "criteria": ["One short source read", "Several additional investigation steps"]
    }
  }
}
```

This example records comparison only; the mandatory source-read rule still determines the action. If that rule already settles the question, set `bypass_reason` to `explicit_rule` and omit the questions.

Input limit: 32,000 bytes; 1–8 questions per eligible checkpoint; 2–8 choices or score levels. All question instructions and criteria are supplied by the caller. Score positions start at zero and can be fractional; Boolean maps to native `noul`. Probabilities, confidence, and scores are distinct measurements. Absent distributions and usage remain `null`, not zero.

The result includes `call_id`, `record_path`, `shadow`, `status`, `answers`, and `action` (the baseline). `status` is `evaluated`, `bypassed`, or `fallback`. Invalid responses, transport/credential errors, and returned model drift use the normal baseline path and record a fallback reason. Invalid input is rejected before sending. No confidence threshold promotes a v2 checkpoint automatically.

The baseline is persisted before evaluation and is not sent to Jev, avoiding anchoring on the agent's intended action. A `recommendation` Choice whose keys match `baseline_action` enables disagreement counts. Other typed answers remain available for independent assessment.

## Outcomes and reports

Save this as `outcome.json`, substituting the returned call ID:

```json
{
  "call_id": "00000000-0000-0000-0000-000000000000",
  "action_taken": "inspect",
  "outcome": "correct",
  "evidence": "An independent source inspection established the relevant handler.",
  "total_decision_ms": null,
  "rework_ms": null,
  "baseline_ms": null
}
```

```sh
scripts/harness advise --record outcome.json
scripts/harness advise --report
python3 ~/.agents/skills/typesafe-routing/scripts/route.py report
```

Labels: `correct`, `incorrect`, `over_escalated`, `under_escalated`, `unknown`. Label the batch as correct only when every material answer is supported; preserve question-level details in the concise evidence. Bypassed or failed evaluations can only receive `unknown`. Each outcome must reference a completed local checkpoint; duplicate labels are rejected instead of silently overwriting evidence.

Private checkpoint files live in `.harness-db/advice/`, outcomes in `.harness-db/advice-outcomes/` (directories 0700, files 0600). `--db-root` selects the database. API call summaries also use the existing private TypeSafe daily logs; deterministic bypasses are local only. The existing `route.py prune` covers daily API logs; local harness checkpoint retention is operator-managed, like other harness database records.

Reports separate family, declared question/policy versions, exact question hash, and requested/returned model. They show labeled accuracy with its denominator, recommendation disagreements, unresolved outcomes, fallback/bypass counts, known token totals, timing sample counts, and medians. Unknown means unmeasured. Total decision time should include context preparation and subsequent work; record rework separately. Only record `baseline_ms` for a comparable measured baseline. Paired timing differences are descriptive, not causal savings from a shadow run.

The shared report distinguishes routing calls, dynamic advice, model probes, and dry runs. Its routing cohorts must not be pooled for threshold tuning. Task-entry activation requires matched current model and policy evidence; historical unversioned records and advice do not qualify.

## Pilot and acceptance

Collect the first 30 independently labeled in-session decisions across these families, including ambiguous and missing-evidence cases. Review disagreements and total overhead before extending automatic use. Thirty cases are an initial review batch, not statistical proof or a promotion switch. Keep a separate held-out set for future threshold evaluation. Changing the model, questions, or policy requires fresh comparable evidence. There is no automatic promotion, model switch, or new runtime in this release.

References: [use cases](https://vercel.com/i/jev-use-cases), [probabilities and thresholds](https://vercel.com/i/jev-probabilities-and-thresholds), [native primitives](https://docs.typesafe.ai/introduction).
