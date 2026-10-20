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

## Bounded automatic-tool context

Automatic tool checks now use `tool-decision-4` / `tool-context-2`. The local
`scripts/jev_context.py` builder supplies the curated task description (or plan
reason), current plan purpose, active todo and criterion, action intent, completed
step evidence, blocked steps, recent read-result kinds/statuses, and recorded
verify/review results. Unknown values stay unknown. Agent-recorded evidence is
identified as such; read counts and returned tools do not prove correctness, and
old checks are not asserted fresh. Provide a short non-sensitive purpose through
`harness workflow task --name ... --description ...` and the todo plan's `--reason`.

Shell inputs are parsed locally into fixed action labels; arguments and command
strings are not sent. Patch headers yield only artifact types, operation kinds
and counts. Source, diffs, tool output, captured prompts, transcript contents and
repository paths are excluded. Curated prose is bounded and redacted for common
credential, contact, address and name patterns; multiline/code-like bodies are
withheld. Redaction is pattern-based, so agents must never put personal data or
secrets into task/todo purposes or evidence. The existing provider-input rejection
remains a second check. This context does not authorize actions or waive gates.

Audit events record `interaction_origin` and `context_version`, not the new prose
facts. Origins are `automatic_tool`, `explicit_decision`, `direct_advice`,
`worker_selection`, `session_routing`, and `task_routing`. Workbench and grouped
APIs separate those origins and context versions alongside the existing family,
question, policy and model dimensions, with confidence samples/means and the
existing usage/cost coverage. Historical origins remain unknown; original events
are not rewritten. New measurements must establish any improvement—more context
is not a claim of greater confidence or correctness. The threshold stays at 70%.

### Action findings and prerequisites

Before an action whose purpose or evidence is not apparent from tool metadata,
provide curated context for the active todo:

```sh
scripts/harness workflow todo context \
  --action-summary "Store absent provider costs as unknown" \
  --finding "Inspected provider responses omit the optional cost field" \
  --gap "Dashboard rendering for missing values remains unverified" \
  --reason "Describe the next proposed behavior change"
```

Replace this context before a different proposed operation. It is scoped to the
current prompt, active todo and plan revision; a new prompt, todo transition or
plan revision makes it unavailable. Starting a new task clears it. Findings and
gaps are optional repeated flags, limited to four each and 180 characters after
redaction; the behavior summary is limited to 220 characters. Supply only
non-sensitive prose, never raw source, diffs, commands, output or personal data.
The local context is redacted before storage and again before provider use.
Audit events record only that context was updated, not these new prose fields.

Automatic inputs identify inspection, implementation, verification, publication
or unknown action types using local tool metadata. These are observations, not
authorization. Agent-supplied findings remain explicitly unverified; an omitted
gap means none was reported, not that prerequisites are satisfied. Generic read
success cannot establish what evidence proves.

`tool-context-2` / `tool-decision-4` evaluates sufficient immediate support,
a specific missing prerequisite resolvable by a read, or
`insufficient_information` when the action/findings are underspecified. In active
mode a confident insufficient-information result requests the command above;
a confident gather-context result requests relevant reads. The existing 70%
threshold, below-threshold main-agent decision and deterministic gates remain.
Checks that can only follow the action remain later obligations. Versioned
Workbench cohorts distinguish these calls without rewriting historical events.

### Two context requests, then the agent decides

Automatic tool checkpoints may enforce at most **two** Jev requests for more
context per native session, prompt, task and active todo. Both `gather_context`
and `insufficient_information` consume the same budget. The first two accepted
requests block the proposed call and show `context request 1/2` or `2/2` with
instructions to supply findings or perform relevant reads. If Jev accepts the
revised action earlier, it proceeds without asking unnecessary questions.

The next execution attempt after two requests uses the main agent without a
third Jev call. Its receipt records `context_request_limit` and the normal
agent-resolution/claim/outcome lifecycle. It is not permission to skip a todo,
phase, authorization, failure-handling rule or required verification.

The counter persists privately under `decisions/context-rounds/`, protected by
an interprocess lock. Cached retries count too, so identical repeated calls
cannot form an endless denial loop. Changed commands, reads, supplied findings,
plan revisions and hook-process restarts do not refill the budget. A different
prompt, task or active todo gets its own budget; returning to an earlier todo
within the same prompt retains that todo's count.

`typesafe.context_request_limit` (0 to 2, default 2) lowers that budget. At `0`
no automatic `gather_context` or `insufficient_information` result blocks a
call: the main agent decides on the first attempt and the receipt still records
`context_request_limit`. Use it when the checkpoint has been blocking the very
reads that would supply its context, such as a connector search whose result is
the missing evidence.

Reads never reach the checkpoint. Besides the built-in read tools and read-only
shell commands, `typesafe.read_only_commands` lists extra read-only shell
commands as word prefixes (for example `career-scheduler phone text` or
`gog gmail search`) and `typesafe.read_only_tools` lists extra read-only tool
names or `__suffixes` (for example `search_threads`). Both count as evidence
reads and skip the decision seam. Never list a command or tool that submits,
writes, sends or deletes: the lists are trusted as declared.

Below **70%**, with missing/invalid confidence, or when Jev is unavailable, the
agent decides immediately without consuming a context request. A configured
confidence threshold may be stricter than 70%, but cannot lower that floor.
This floor applies to explicit advice, decision receipts and task-entry routing.
Policy cohorts now use `decision-3`; context/question versions remain unchanged
because the provider question did not change.
