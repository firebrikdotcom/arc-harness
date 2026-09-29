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

## Automatic checkpoints at harness seams

Hand-authored checkpoints produced almost no evidence, so the harness now emits shadow checkpoints itself wherever it already has a deterministic oracle. `scripts/phase_checkpoint.py` derives enum-only signals and never includes paths, prompts, diffs, command text, run ids, or hashes. Everything is gated by `HARNESS_JEV_CHECKPOINTS=1` (exported by `scripts/jev-enable.sh`); without it every call is a silent no-op, and nested harness tests always run with it off.

| Seam | Family / question version | Baseline | Oracle that labels it |
| --- | --- | --- | --- |
| `harness plan done` | `handoff_assessment` / `phase-plan-2` | `proceed_to_build` | `review done`: did the run re-enter an earlier phase? |
| `harness build start` | `reasoning_allocation` / `phase-build-2` | `routine` | the first `verify.sh` exit after build start |
| `verify.sh` before checks | `evidence_assessment` / `verify-predict-2` | `run_full_verification` | that verification's exit code |
| `review.sh` after verification | `handoff_assessment` / `review-handoff-2` | `ready_for_handoff` or `needs_more_work` | `review done`: loop count since the checkpoint |
| third identical shell command (hook) | `progress_assessment` / `tool-repeat-2` | `retry_same_command` | whether the command recurred, or the run looped, before its phase and run ended |
| session start in a harness target (hook) | task-entry route (`scripts/harness route`, shadow) | existing command | none automatic |

### Facts every automatic checkpoint receives

Every `-2` checkpoint shares one fact set of buckets, small counts (loops, windows of at most five, failed verifications in the run, complete retrievals), and yes/no values:

- Working tree: dirty file bucket; changed-line bucket (`none`, `small` ≤50, `medium` ≤300, `large` ≤1500, `very_large`) from `git diff HEAD --numstat` plus the lines of untracked files up to 1 MB each; the top three languages; the change shape (`source_and_tests`, `source_only`, `tests_only`, `docs_or_config_only`, `none`), which says whether tests moved together with source rather than merely whether a test file changed (a test file is one under a `test`/`tests`/`spec`/`__tests__` directory or named like `test_*`, `*_test.*`, `*.test.*`, `*.spec.*`, `conftest.py`, or `FooTest.*`); whether documentation changed; whether the project has a test directory; harness steps and loops used.
- Verification history of the target: of the last five verifications, how many passed and failed, whether the most recent passed and how many in a row; verifications so far in this run and how many failed; and whether the working tree changed since the most recent verification (`yes`, `no`, or `unknown`).
- Previous runs of the target: completed, aborted, and unfinished counts (bucketed), and how many of the last five completed runs re-entered an earlier phase.
- Semantic retrieval through `scripts/jg.sh` in this run: a count bucket and how many results were complete, from the `retrieval/` records of the same run id; those records hold no question text, paths, or excerpts.

The verification history is a private file, `records/verify-history.jsonl` (0600), to which the `verify-result` event appends one line per verification: epoch, exit code, failure and check counts, the run id (or none), and a local sha256 fingerprint of HEAD, the tracked diff, and the untracked files' contents. It keeps the last 100 lines and accumulates only while checkpoints are enabled. When `records/verify.state` is newer than the history's last line (a database from before the history existed, or a verification that ran with checkpoints disabled), it is counted as the most recent result, with an `unknown` tree comparison. The fingerprint and run id stay local; only the yes/no comparison is sent. The tool-repeat checkpoint also states how many shell commands the run has recorded and how many were distinct.

### Why the handoff questions changed

`phase-plan-1` asked whether the plan was recorded in `progress.md`, `review-handoff-1` whether documentation and `progress.md` were current, and both saw the fact "progress.md changed". Interactive targets never write `progress.md`, so Jev correctly answered no and recommended holding, and the loop oracle then labeled nearly every hold over-escalated. Having `plan done` write a run-scoped plan note was considered and rejected: the harness would be answering its own question, a constant "yes" that carries no information about the outcome the oracle measures. The `-2` handoff checkpoints drop the `progress.md` questions and fact and ask `loop_likely` next to the go/hold recommendation, the thing the oracle actually observes; `clarification_needed` is dropped because no enum fact can speak to a user-owned decision. The build-start, verify, and tool-repeat questions are unchanged, but their facts changed, so they move to `-2` as well: cohorts of different versions must not be pooled.

### Mechanical labels

Labels are mechanical and documented so they stay comparable: a pass prediction (`will_pass` probability at or above 0.5) that fails is `under_escalated`, a fail prediction that passes is `over_escalated`, a match is `correct`; `routine` is correct only when the first verification passes and `deep_reasoning` only when it fails; a go recommendation (`proceed_to_build`, `ready_for_handoff`) is correct only when no later loop happened, and a hold recommendation only when one did. The verification inside `review.sh` produces its own prediction and label, so a clean run yields five labeled decisions, six when a tool repeat occurred. Pending oracles wait under `.harness-db/advice-pending/` and are removed once labeled.

Tool repeats: the observe hook appends a checksum of every non-harness shell command to the run's `command-digests` log and calls the checkpoint on the third identical one. The pending oracle records the checksum's position in that log (locally, never sent). Every later checkpoint event of any run resolves it:

- stuck, when the same checksum appears again after the checkpoint, or the run's loop count has risen since it: `change_approach` and `gather_more_evidence` are `correct`, `retry_same_command` is `under_escalated`;
- not stuck, when the run has ended (completed at `review done`, aborted, or superseded by a newer run) and the phase in which the command repeated had completed, with no recurrence and no loop: `retry_same_command` is `correct`, the two hold choices are `over_escalated`;
- `unknown`, when the run ended before that phase completed; otherwise it keeps waiting.

Repeats that are routine (running the same test command after each edit) therefore count as not stuck unless the checksum recurs again; the oracle cannot see edits between identical commands, so a recurrence is always read as stuck.

Run scoping: an oracle is labeled only from the run that created it. Pending items left by a run that ended without reaching its oracle are labeled at the next checkpoint event from that run's own state: a handoff whose run completed or looped uses that run's loop count, and anything else is `unknown`. A later run's verification or loop count never labels another run's checkpoint. Existing pending items already carry the run id that created them, so `-1` items still waiting are run-scoped too; only an item with an empty run id keeps the unscoped behaviour. Malformed integer fields of a pending item (such as a hand-edited `loops_at`) read as zero, so the item is still labeled or keeps waiting. An item is dropped with a `could not label` line only when labeling it fails, for example because its advice record was removed by retention or its call id is missing or invalid; a pending file that is not a JSON object is dropped outright. A failure while resolving earlier oracles never suppresses the event's own checkpoint or another item's label. `unknown` labels do not count toward the pilot.

`harness review done` prints the pilot counter (labeled decisions out of 30) and any unlabeled evaluated checkpoints. `scripts/harness advise --pending` lists them at any time; `advise --report` includes the same `pilot` summary.

Every git worktree is its own target with its own database, so a per-target count would never reach 30. The pilot counter and `advise --report` therefore default to **machine scope**: they sum the `advice/` and `advice-outcomes/` directories of every target in the registry (`scripts/harness-target.sh list`) that has checkpoints, and `review done` prints that machine-wide number. `--scope target|machine` selects the scope explicitly (`--report` defaults to `machine`; `--pending` defaults to `target`, because a label is recorded in the database that owns the checkpoint, and `--pending --scope machine` names each row's `target`). The JSON keeps the fields consumers already read (`pilot.labeled`, `correct`, `remaining`, `review_batch_ready`, `by_family`) and adds `scope`, `by_target` (labeled, correct, unlabeled, fallback and bypassed counts per registry id, plus `legacy` for the pre-registry shared database beside `targets/`) and `unreadable_records` (other targets' malformed files are skipped and counted; the current target's still fail loudly). Report cohorts remain keyed by family, question/policy version, question hash and model, so machine scope pools only identical cohorts and never mixes versions for tuning. Registry ids are local names and appear only in this local output, never in checkpoint facts or telemetry.

## Flag form

`scripts/harness advise --family FAMILY ...` builds and validates the same v2 payload without a JSON file:

```sh
scripts/harness advise --family tool_selection --baseline inspect \
  --goal "Choose the next read-only step to locate a missing handler." \
  --fact "The graph result is stale; the current source is available." \
  --constraint "Existing graph coverage rules remain mandatory." \
  --choice inspect="Read the current candidate source" --choice trace="Trace a related caller" \
  --boolean sufficient="The supplied evidence establishes the handler location." \
  --score impact="How much investigation would a wrong next step waste?:One short source read|Several additional investigation steps"
scripts/harness advise --label CALL_ID --outcome correct --action-taken inspect \
  --evidence "An independent source inspection established the relevant handler."
```

`--bypass REASON` records a deterministic bypass with no questions; `--question-version` and `--policy-version` name the cohort (defaults `quick-1` and `shadow-1`). Redaction, size limits, and shadow semantics are identical to the file form.

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

Private checkpoint files live in `.harness-db/advice/`, outcomes in `.harness-db/advice-outcomes/`, and unresolved automatic oracles in `.harness-db/advice-pending/` (directories 0700, files 0600). When `HARNESS_AUDIT_ENABLED=1`, each evaluated checkpoint and each label is also mirrored to the Arc service as `jev.checkpoint` and `jev.checkpoint_outcome` with compact facts and a hashed evidence digest. `--db-root` selects the database. API call summaries also use the existing private TypeSafe daily logs; deterministic bypasses are local only. The existing `route.py prune` covers daily API logs; local harness checkpoint retention is operator-managed, like other harness database records.

Reports separate family, declared question/policy versions, exact question hash, and requested/returned model. They show labeled accuracy with its denominator, recommendation disagreements, unresolved outcomes, fallback/bypass counts, known token totals, timing sample counts, and medians. Unknown means unmeasured. Total decision time should include context preparation and subsequent work; record rework separately. Only record `baseline_ms` for a comparable measured baseline. Paired timing differences are descriptive, not causal savings from a shadow run.

The shared report distinguishes routing calls, dynamic advice, model probes, and dry runs. Its routing cohorts must not be pooled for threshold tuning. Task-entry activation requires matched current model and policy evidence; historical unversioned records and advice do not qualify.

## Pilot and acceptance

Collect the first 30 independently labeled in-session decisions across these families, including ambiguous and missing-evidence cases. The automatic seams supply most of them; `HARNESS_JEV_TIMEOUT` (default 10 seconds per attempt) and `HARNESS_JEV_ATTEMPTS` (default 1 for automatic checkpoints) bound the latency they add to phase gates, verification, review, and hooks. Review disagreements and total overhead before extending automatic use. Thirty cases are an initial review batch, not statistical proof or a promotion switch. Keep a separate held-out set for future threshold evaluation. Changing the model, questions, or policy requires fresh comparable evidence. There is no automatic promotion, model switch, or new runtime in this release.

References: [use cases](https://vercel.com/i/jev-use-cases), [probabilities and thresholds](https://vercel.com/i/jev-probabilities-and-thresholds), [native primitives](https://docs.typesafe.ai/introduction).
