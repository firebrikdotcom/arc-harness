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
   `scope` and `state` (no prior baseline); see below. Batch independent judgments in the
   existing `harness advise` interface when appropriate.
3. Submit a role plan with `harness dispatch create --plan PLAN.json --project PATH`.
   For a trivial task use `workers: []` with a rationale and acceptance criteria;
   substantial work requires the explicit assessment below, including justified main-only work.
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
| `typesafe.mode` | `off`: no provider calls; `shadow`: observe advice; `active`: follow confidence-gated choices. |
| `typesafe.enforce_decisions` | Add decision receipts to covered execution hooks; dispatch independently requires the current prompt role plan. |
| `typesafe.decision_ttl_seconds` | Cache exact unchanged decisions within this interval; execution receipts are single-use. |
| `dispatch.enabled` | Permit role dispatch. Disabling it prevents new launches; existing workers remain visible and cancellable. |
| `dispatch.max_workers` | Shared native/external capacity per run; native runtime limits may be lower. |
| `dispatch.max_context_bytes` | Maximum worker packet size in UTF-8 bytes. |
| `dispatch.timeout_seconds` | External process timeout and native handoff deadline; external process supervision currently requires Linux /proc. |
| `dispatch.profiles` | Operator-configured role, provider, model and reasoning effort combinations. |
| `amp.enabled` | Offer every dispatched worker an Amp orb first ([Amp orb workers](#amp-orb-workers)). Off by default. |
| `amp.effort` | Effort for orb workers; it picks the Amp mode. Default `medium`. |
| `amp.poll_seconds` | Seconds between checks of a running orb thread. |
| `telemetry.*` | Master and category collection switches; does not change execution permissions. |

The supplied profiles cover native exploration, implementation and review, plus
Codex implementation and Claude review; `amp.enabled` adds the orb profile in front of
them. Add a `command` provider with a fixed
argv array for another installed agent or model. Only profiles named in the
worker's eligible list can be selected. Worker plans cannot inject executables.
Model availability follows the actual runtime/account; an unavailable model is
a recorded worker failure, not permission to silently change providers.

## Worker plan

```json
{
  "assessment": {
    "task_size": "substantial",
    "strategy": "delegate",
    "reason": "One independent inspection can run while the parent implements.",
    "model_fit": "A smaller native model can inspect this bounded concern; the parent verifies its evidence.",
    "latency_tradeoff": "Parallel inspection offsets packet setup and verification overhead."
  },
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

`schemas/dispatch.schema.json` describes the complete shape.

### Required delegation assessment

Before substantial work, explicitly consider bounded assignments, parallel work,
smaller-model capability, native tool availability, startup/context overhead and
parent verification/rework. Choose `delegate` only when the expected benefit
justifies that overhead; otherwise choose `main_only` with concrete reasons.
Prefer smaller native workers when their capabilities fit. Do not delegate tiny
operations just to satisfy a quota, or repeat a worker's entire investigation.

The dispatcher requires `assessment` whenever workers are present or the current
Workflow plan has more than one required, non-removed step (completed steps still
count). A single step can also be substantial: declare it explicitly. This is an
observable scope rule, not a claim to infer semantic complexity. Trivial main-only
plans retain the short `workers: []`, `rationale`, `acceptance` form. An explicit
assessment has exactly the five fields in the example; `task_size` is `trivial`
or `substantial`, and `strategy` is `main_only` or `delegate`, matching the worker
list. Text fields are nonempty, at most 1000 UTF-8 bytes each; keep them redacted.
Main-only plans still need their rationale and acceptance criteria.

Assessments persist locally with the run. Telemetry sends only size and strategy,
never the free-text reasons. Changing required todo scope invalidates an old role
plan; changing a todo's status does not. Existing legacy plans remain usable only
when no required execution steps are declared.

Editing workers
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

## Amp orb workers

On a machine where `amp.enabled` is on, every dispatched worker is offered an Amp orb
first: a remote machine that clones the target's Git remote through its Amp project.
With it off, nothing changes. Each machine decides for itself, because the switch lives
in that machine's `config.json`.

When the switch is on, the dispatcher gives each worker the orb profile (`amp_orb`:
provider `amp`, effort `amp.effort`) before considering the profiles the plan lists. It
falls back to those listed profiles, recording why in the worker's `orb_skipped`, when
an orb would not see what a local worker sees: the `amp` CLI is missing, the worker
depends on other workers (their results exist only locally), the current commit is not
pushed, a tracked file has uncommitted changes, or the worker's owned paths have new
files. This applies to read workers (explorers, reviewers) as well as edit workers,
and the fallback never picks an amp profile from the list; a worker that lists only
amp profiles is refused at create with the reason. The checks run when the plan is
created and again at launch. Once an orb has launched, a failure (for example an
expired `amp login`) is a visible worker failure, not a silent switch. A plan may also
list an explicit `"provider": "amp"` profile; that still requires `amp.enabled`.

The orb has no harness, so it gets only the bounded packet, and nothing it
does counts until the parent checks it here. Read workers report back; edit workers
push a branch that the parent fetches into the worker's own worktree. Integration,
`verify.sh`, acceptance commands and independent review then run locally as usual.

Before using it:

- Log in with `amp login`, and make sure the target's remote matches an Amp project
  (`amp projects status` in the target checkout).
- Push and commit before dispatching. An orb only sees what is on the remote, so
  workers fall back to local profiles while `HEAD` is unpushed or tracked files are dirty.
- Turn the switch on, and optionally pick the orb effort:
  `scripts/harness config update --section amp --values '{"enabled":true,"effort":"medium"}'`.
  Role dispatch itself must also be on (`dispatch.enabled`).

Orbs start only through the dispatcher. `scripts/install-hooks.sh` denies Claude Code
running `amp -ox` or `amp --orb-execute` itself (and removes an older allow rule), the
guard refuses direct orb launches and runs of the prompt hook script, judged on the parsed
command so wrappers such as `env`, `nohup`, `timeout`, `xargs` and `sh -c` do not hide
them, and agents cannot redirect the workflow state, dispatch database or config the
dispatcher reads, or set session IDs. Every orb therefore leaves a dispatch record and
a saved thread export.

### How it runs

`dispatch execute` sends the packet on stdin to `amp -ox` (stdin because execute
mode times out reading a long argument), with the mode taken from the profile effort
(`xhigh` becomes `high`, `max` becomes `ultra`), the title `harness RUN WORKER` and the
label `harness-dispatch`. It records the thread URL, then polls `amp threads export`
every `amp.poll_seconds` until the last assistant turn has ended and the thread is
idle, or `dispatch.timeout_seconds` passes. No local process is supervised, so orb
workers also run where Linux `/proc` is unavailable.

The packet replaces the local workspace path with the orb checkout. A read worker is
told to check out the dispatched commit (`git checkout --detach SHA`) and not to modify,
commit or push. An edit worker is told to check out
`harness/RUN-WORKER` from the dispatched commit, commit only owned paths, push that
branch and report the pushed SHA as `commit`. The parent fetches the branch, requires
it to descend from the dispatched commit and to match the reported SHA, and applies
its diff to the worker worktree. The usual ownership check then rejects any path the
worker does not own.

### What is audited

Each report must also carry `audit`: commands with exit codes, files written,
hosts contacted and deviations. Because a worker's own account can be incomplete,
the full thread export is saved as `dispatch-artifacts/RUN/WORKER.thread.json`
(0600) and the worker result keeps:

- `audit.thread_url`, `audit.export` and `audit.export_sha256`;
- `audit.reported`: the orb's own `audit` object;
- `audit.observed`: tool-call counts, shell commands and files named by edit tools,
  taken from the export;
- `audit.unreported_files`: observed files missing from the reported list (recorded,
  not failing: files written by shell commands are not observed);
- token usage summed from the export's per-turn usage, when present.

### Limits

- Cancelling an orb worker stops tracking it locally; the thread itself keeps running.
- A timed-out worker fails with exit code 124 and the thread URL; the thread may still finish.
- If the supervisor (`dispatch execute`) itself dies, `dispatch status` marks the worker failed
  only after `dispatch.timeout_seconds` plus 10 minutes.
- Pushed `harness/...` branches stay on the remote until someone deletes them.

## Explicit decision lifecycle

```json
{
  "family": "context_selection",
  "goal": "Choose the next bounded source of evidence.",
  "facts": ["One candidate symbol is known; its source has not been inspected."],
  "choices": {"inspect": "Inspect the candidate source", "trace": "Trace a related caller"},
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
internal reasoning and handoffs outside those tools. Active recommendations require `typesafe.min_confidence` (default 0.70); below it, or when Jev is unavailable, the main agent decides. For an explicit decision, resolve the fresh escalated receipt with `harness decision resolve --id ID --action OPTION --reason REASON`; unchanged evaluations reuse that resolution. Covered tool hooks record the agent's proposed tool call as its decision, and dispatch uses the first profile the plan lists for that worker. No prior agent baseline or routine accuracy label is required. A changed prompt, plan, evidence,
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

### Whole-task observations

After workers terminate and the parent finishes integration, verification and
review, record one observation for the role plan (also for main-only work):

```sh
harness dispatch measure --run RUN --result /dev/stdin <<'JSON'
{
  "comparison_key": "bounded_bug_fix",
  "total_tokens": null,
  "cost_usd": null,
  "rework_count": 0,
  "quality_passed": true
}
JSON
```

Token and USD totals must include parent and workers, routing, failed attempts
and review; omit them or use null unless the whole scope is measured. Do not
substitute worker-only totals or infer cost from token count. `rework_count`
counts corrective cycles after a candidate fails checks or review; unknown is
null. `quality_passed` reports whether required checks and independent review
passed, without replacing those gates. Zero is a known measurement, not a
missing value. A non-sensitive `comparison_key` groups comparable work, using
1–64 letters, digits, underscores or hyphens; it is not a task description.

Elapsed milliseconds are measured from role-plan creation to this observation,
including parent overhead and waiting. Record promptly after the checks, once
per run; late observations include the delay. Plan setup before creation is not
measured. A measurement does not complete a Workflow task or waive any gate.
Measurements are explicit parent observations, not automatically recovered
native-provider billing. The existing orchestration API exposes `task_measurements`
by comparison key and strategy, with sample counts, unknown coverage, elapsed
time, tokens, USD cost, rework and quality. Legacy runs retain unknown strategy
and measurements. Compare matched task cohorts before requiring broader
subagent use; no automatic policy promotion or claimed speedup is introduced.

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
