# Role dispatch and decision checkpoints

The parent agent owns the task, integration and final verification. Every worker
has one role, a bounded responsibility, acceptance criteria, owned paths and a
small context packet. Never copy the conversation history into worker prompts.

## On each prompt

1. Confirm or revise the audited todo plan. Apply explicit instructions and
   deterministic permissions, failure handling and required checks first.
2. Use `harness decision evaluate --input DECISION.json --project PATH
   --session-id SESSION` for each remaining bounded judgment: decomposition,
   priority, role/model/provider selection, evidence, context, escalation and
   handoff. The input supplies `family`, `goal`, redacted `facts`, `choices`,
   `baseline`, `scope` and `state`; see below. Batch independent judgments in the
   existing `harness advise` interface when appropriate.
3. Submit a role plan with `harness dispatch create --plan PLAN.json --project PATH`.
   For a trivial task use `workers: []` with a rationale and acceptance criteria;
   deciding whether delegation is useful does not require launching a worker.
4. Use `harness dispatch next --run ID` and start its native and external tool
   handoffs concurrently. External handoffs call `dispatch execute`; native
   handoffs supply exact spawn arguments. `dispatch run` is a synchronous
   convenience runner; use `next` for mixed native/external parallelism.
   Continue useful independent work while workers run.
5. Check evidence, integrate completed patches, run the project's actual checks
   on the combined result and obtain independent review. A successful worker
   process or a completed report never replaces project verification.

## Configuration

All persisted switches and eligible worker profiles share
`~/.config/harness/config.json`, including short `_doc` reminders.

| Setting | Effect |
| --- | --- |
| `typesafe.mode` | `off`: no provider calls; `shadow`: compare with baseline; `active`: follow eligible choices. |
| `typesafe.enforce_decisions` | Add decision receipts to covered execution hooks; dispatch independently requires the current prompt role plan. |
| `typesafe.decision_ttl_seconds` | Cache exact unchanged decisions within this interval; execution receipts are single-use. |
| `dispatch.enabled` | Permit role dispatch. Disabling it prevents new launches; existing workers remain visible and cancellable. |
| `dispatch.max_workers` | Shared native/external capacity per run; native runtime limits may be lower. |
| `dispatch.max_context_bytes` | Maximum worker packet size in UTF-8 bytes. |
| `dispatch.timeout_seconds` | External process timeout and native handoff deadline; external process supervision currently requires Linux /proc. |
| `dispatch.profiles` | Operator-configured role, provider, model and reasoning effort combinations. |
| `telemetry.*` | Master and category collection switches; does not change execution permissions. |

The supplied profiles cover native exploration, implementation and review, plus
Codex implementation and Claude review. Add a `command` provider with a fixed
argv array for another installed agent or model. Only profiles named in the
worker's eligible list can be selected. Worker plans cannot inject executables.
Model availability follows the actual runtime/account; an unavailable model is
a recorded worker failure, not permission to silently change providers.

## Worker plan

```json
{
  "workers": [
    {
      "id": "inspect",
      "role": "explorer",
      "responsibility": "Trace the affected execution path and return evidence.",
      "acceptance": ["Identify the owning function and relevant regression coverage"],
      "owned_paths": ["src"],
      "context": "Only inspect the subsystem named in the task; return source references.",
      "dependencies": [],
      "access": "read",
      "profiles": ["native_scan"]
    }
  ]
}
```

`schemas/dispatch.schema.json` describes the complete shape. Editing workers
receive separate sibling Git worktrees based on the current parent workspace,
including permitted dirty/untracked source changes. Sensitive files and private
runtime state are excluded. Original work remains in the parent checkout.
Conflicting ownership and dependency cycles are rejected before execution.
Failed dependencies block their dependents. Retain worker worktrees and patches
until their output is verified; cleanup must not discard unreviewed work.

## Native bridge and external workers

Native tools belong to the interactive parent runtime. Python cannot invoke
`collaboration.spawn_agent` itself. The dispatcher returns the real spawn
arguments with `fork_turns: none`, selected role/model/effort, workspace and
bounded message. Execute that tool, then record its actual ID:

```sh
scripts/harness dispatch bind --run RUN --worker inspect --agent-id ACTUAL_ID
scripts/harness dispatch complete --run RUN --worker inspect --result report.json
scripts/harness dispatch status --run RUN
scripts/harness dispatch cancel --run RUN --worker inspect
```

`message` returns a native message handoff. Cancellation and expired native
deadlines return interrupt handoffs: the parent must execute them, then acknowledge
with `dispatch cancel --run RUN --worker WORKER --acknowledged --evidence TEXT`.
Capacity remains reserved until acknowledgement. A native
thread's sandbox and tools are controlled by its runtime; the dispatcher does
not elevate them or claim it can stop an unresponsive native runtime itself.

External profiles use exact argv execution, bounded process groups and the
runtime's normal permissions. They do not use approval/sandbox bypass flags.
Inherited parent session IDs are removed. Each external worker keeps harness bookkeeping
in its own private workspace directory; it cannot mutate the parent run state.
Task prompts are explicit arguments and external stdin is disconnected.
Errors, invalid reports, cancelled
workers and unavailable models remain visible. Editing outputs are patches;
integrate completed outputs with `harness dispatch integrate --run RUN --worker WORKER`;
it verifies patch identity/ownership and stops on conflicts, preserving parent edits.
Dependents receive predecessor patches in their own isolated baseline.

Reports contain `status`, overall `evidence`, `artifacts` and an ordered
`acceptance` array with `criterion`, `passed` and concrete `evidence` for each
criterion. The parent verifies these observations before submitting completion.
Token usage and cost are optional measurements and remain unknown when the
runtime does not expose them. Packet bytes measure the supplied role/context
envelope; runtime token usage also includes its ambient system/project instructions.
For speed, prefer native workers when their model and tools fit: an external
runtime still loads its mandatory ambient instructions and may spend more tokens
on setup than on a small task. Cached input is measured separately where exposed.
Codex workers omit the optional skill catalog and apps/plugins/memories, retaining
project/user instructions, hooks and permission rules.

## Explicit decision lifecycle

```json
{
  "family": "context_selection",
  "goal": "Choose the next bounded source of evidence.",
  "facts": ["One candidate symbol is known; its source has not been inspected."],
  "choices": {"inspect": "Inspect the candidate source", "trace": "Trace a related caller"},
  "baseline": "inspect",
  "scope": {"action_hash": "LOCAL_ACTION_DIGEST"},
  "state": {"evidence_revision": 1}
}
```

Bind scope to the current prompt's `request_id`, `request_seq` and
`plan_revision` when those exist in the Workflow session. Claim the receipt with
`harness decision consume` using its exact scope, state and action fingerprint;
record the observed result with `harness decision outcome`. The result is a
routing decision, never an authorization or a waiver of mandatory checks.
Automatic hooks do this for covered execution tools; explicit decisions cover
internal reasoning and handoffs outside those tools. A provider outage keeps
the baseline and records the fallback; retries on unchanged state reuse that
outcome during its bounded cache interval. A changed prompt, plan, evidence,
options, model or mode requires a fresh decision.

## Measurement and dashboard

Open `/orchestration` on the existing audit service, or query
`/api/audit/orchestration`. The page shows a direct parent-to-child diagram. The selector contains session
names only and changing it immediately opens that session, with no submit button.
The parent box shows its name and a small session-ID subtitle; each arrow branch
ends at a compact child-model box. Expand a model box for recorded worker identity,
status and measurements. Dispatch runs and decision checkpoints are not diagram
nodes. Session details and the complete measurement API remain linked below.
Names come from compact Workflow reports joined by exact session ID. Children
are selected by their recorded parent ID; run IDs are not needed to draw that
relationship, and identical models or worker names never merge separate jobs.
Observations without parent IDs remain unlinked, without connecting arrows.
The view draws up to 200 children and reports omitted counts. Missing child session
IDs and measurements stay explicitly unknown in expanded details.
The JSON API retains all aggregate records and measurements.
 Concurrency is calculated only from measured complete intervals. Unknown
usage/cost is distinct from zero. No speedup is claimed without a comparable
measured baseline.

Events contain compact facts, identifiers and measurements, not worker prompts,
source, command output or diffs. Delivery uses the existing durable outbox; an
unavailable dashboard queues events without changing execution decisions.

## Enforcement boundary

Hooks enforce observable supported tool seams, prompt/plan freshness and native
handoff reporting. They cannot observe private reasoning, intercept tools that
the host does not expose to hooks, guarantee model truthfulness, or replace the
host sandbox. Agent instructions require explicit checkpoints for those
judgments, and the parent remains responsible for evidence and completion.

Regression commands:

```sh
sh tests/decision-checkpoints.sh
sh tests/role-dispatch.sh
cargo test --manifest-path services/harness-audit/Cargo.toml --locked
scripts/verify.sh --project .
```
