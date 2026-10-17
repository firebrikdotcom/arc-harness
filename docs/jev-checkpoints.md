# Jev decision checkpoints

Use Jev first when a bounded judgment remains after explicit instructions and deterministic evidence. Do not decide an agent baseline before asking. Permissions, user choices, required verification and known failures remain deterministic; Jev cannot override them.

## Decision flow

1. Supply a compact, redacted goal, relevant facts and allowed choices. Never send credentials, personal data, source files, raw prompts or full diffs.
2. In active mode, accept a valid Jev recommendation only when its reported confidence is at least `typesafe.min_confidence` (default `0.70`). Confidence is self-reported, not measured accuracy.
3. Below the threshold, the main agent decides. Low, missing or malformed confidence, model drift and provider errors return `normal_reasoning` with `needs_reasoning: true`, and only then does the agent reason about the choice. Where the agent has already expressed that decision, the harness records it instead of asking again: a tool hook runs the tool call the agent proposed, dispatch uses the first profile the agent listed for a worker, and the task-entry launcher keeps the agent's default launch command.
4. Reuse the receipt for unchanged state. A changed prompt, plan, evidence, model, threshold or choice set requires a fresh evaluation.
5. Record execution results where the lifecycle requires them. Do not run an accuracy comparison or label correctness as routine work.

```sh
scripts/harness advise --family evidence_selection \
  --goal "Choose the next useful source of evidence" \
  --fact "One candidate symbol is known; its current source is unread" \
  --choice inspect="Read the candidate source" \
  --choice trace="Trace a related caller"
```

Three useful moments to ask, in flag form. Keep goals, facts and choices redacted: no paths, source, question text, credentials or personal data.

- Before the first broad Grep or Glob in an unfamiliar target: `scripts/harness advise --family tool_selection --goal "locate the code for one task" --choice grep="targeted grep" --choice retrieval="one semantic retrieval first" --fact "target unfamiliar"`
- Before settling a review finding's severity: `scripts/harness advise --family evidence_assessment --goal "grade one review finding" --choice blocker="blocks merge" --choice major="fix before handoff" --choice minor="follow-up" --fact "finding reproduced: yes"`
- Before a handoff with unresolved failures or skipped checks: `scripts/harness advise --family handoff_assessment --goal "decide whether to hand off" --choice hand_off="hand off with the gap stated" --choice keep_working="fix first" --choice ask_user="user decision needed" --fact "failing checks: 1"`

The `--fact` values are examples: replace each with the real redacted fact. Take the returned `action` when it is a choice; when it is `normal_reasoning`, you decide.

`advise --context CHECKPOINT.json` supports batched independent typed questions. For a v2 checkpoint, omit `checkpoint.baseline_action`. Legacy baseline fields remain readable for compatibility but are not needed for the active flow. Dependent questions need fresh evidence.

## Explicit execution receipts

`harness decision evaluate --input DECISION.json` records a scoped decision with `family`, `goal`, `facts`, `choices`, `scope` and `state`. Bind scope to the current Workflow prompt and plan revision. Claim an accepted action with `decision consume` using its exact scope, state and action fingerprint; report observed execution with `decision outcome`.

When a receipt requires agent reasoning, resolve it after that reasoning:

```sh
scripts/harness decision resolve --id RECEIPT_ID --session-id SESSION_ID \
  --action CHOSEN_ORIGINAL_OPTION --reason "Concise evidence supporting this choice"
```

Resolution is available only for a fresh escalated receipt and an original option. It cannot override an accepted Jev recommendation. Re-evaluating unchanged context reuses the resolution with `action_source: agent_after_escalation`, preferring it over any unresolved copy. Tool hooks and dispatch resolve their own escalations with the agent's proposed action, so nothing waits on a manual resolution; the receipt still records the confidence, the escalation reason and who decided.

## Modes and deterministic gates

`harness jev on|off|status` controls delegation. Active mode follows eligible confidence-gated recommendations. Shadow mode observes advice while returning normal reasoning; it does not require an agent baseline or accuracy labels. Explicit off mode makes no provider calls. Configured model pins, operator controls and existing permission gates still apply.

Mechanical phase, verification, review and repeat-command seams no longer request predictions or create automatic correctness labels. Their actual checks and gates still run. Session-start routing remains available for eligible judgments.

## Measurement

Workbench plots daily mean Jev confidence from genuine valid recommendation samples, in the selected browser timezone. Missing confidence remains unknown; zero is a valid sample. The chart does not join agent choices or correctness labels. It is confidence, not observed accuracy.

Private decision and advice records retain confidence, timings and available usage. Historical events, manual outcome APIs and legacy reports remain readable for compatibility; they are not prerequisites for accepting a new recommendation. No pilot accuracy batch is required for the intended active flow.
