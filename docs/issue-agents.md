# Issue agents: investigate production errors from the board

**What it is.** An **Analyze** button on any problem in an architecture board card lets you investigate a production error without leaving the dashboard. One click queues a read-only agent that inspects the code in its own isolated git worktree and writes a structured diagnosis (likely cause, code involved, reproduction steps, suggested fix, blast radius, and confidence) displayed directly on the problem drawer.

**Read-only and self-contained in phase 1.** The agent receives no MCP tools at all (a strict empty MCP configuration). It works entirely from a scrubbed context packet prepared from the error data that `harness-signals` already gathered and stored, so no additional Nightwatch sign-in or external tool credentials are required. It never alters your primary working checkout, commits changes, pushes branches, opens pull requests, or writes to external issue trackers.

**You remain in control.** Every job records the agent kind, session ID, dedicated worktree, branch, host machine, and launcher (a visible Herdr tab or a background process). You can monitor recent actions, start a local diff viewer, stop the agent, or take over the exact session with one command.

---

## Using Analyze

### Starting an investigation

1. In the dashboard, open a project's architecture board (for example, Dollarwise at `/projects/{target}/board`).
2. Click a card with error heat to reveal its **Problems** drawer.
3. Every issue with an in-app code frame displays an **Analyze** button.
   The same button is on the **Agent** tab of the issue sheet, which opens when you pick an issue in the ticker along the top of the board; it starts the same job.
4. Clicking **Analyze** queues a job. If multiple error events share the same innermost line in your application code, they deduplicate into one single job so a recurring bug does not spawn redundant agents.

While an agent is investigating, the card displays an `agent · 1` badge and the issue displays an active `Analyzing` chip.

### The job panel

Clicking an active or diagnosed issue opens its job panel:

- **Where it runs:** Shows the agent kind (`claude` or `codex`), session ID, worktree folder, branch (`nw/<fingerprint>`), machine, diff viewer status, and execution target (a named Herdr tab when Herdr is enabled, or a background process). Session ID and worktree path include quick **Copy** buttons.
- **What it is doing:** A live feed of recent agent actions with timestamps, showing files read and search patterns executed.
- **Actions:** Buttons to run the diff viewer (**Run difit**), stop the agent (**Stop**), or take over the session (**Take over**).

### Taking over and handing back

Two writers on one interactive session would corrupt conversation state, so taking over always stops or detaches runner supervision first:

1. Click **Take over** in the job panel.
2. For background agents, the button reads **Stop after this step and take over**. The job shows `Stopping` while the runner waits for the agent to finish its current step (no tool call open, and its session log quiet for 2 seconds), for at most `issue_agents.takeover_wait_seconds` (30 by default). It then stops the agent, sets `owner: you` and keeps the worktree intact. If no step boundary comes in time, it stops the agent immediately and the job's events say so. Hand back and the resume commands appear once the agent has stopped. For Herdr jobs, taking over stops nothing: the runner releases its supervision while the agent keeps running in its Herdr tab.
3. The panel displays tabbed resume commands tailored for where you are working:
   - **On this machine (local):**
     ```sh
     cd '<worktree>' && claude --resume '<session_id>'
     # or for Codex:
     codex resume '<session_id>' --cd '<worktree>'
     ```
   - **From another machine (SSH):** Prefixes the local command with the configured SSH destination (`issue_agents.ssh` in `signals.json`):
     ```sh
     ssh -t '<destination>' 'cd '\''<worktree>'\'' && claude --resume '\''<session_id>'\'\''
     ```
   - **In a Herdr tab:** Identifies the dedicated Herdr tab name (`issue-<fingerprint8>`) to focus or switch to directly (`herdr agent focus '<name>'`).
4. When you are finished exploring or wish to resume automated tracking, click **Hand back to runner** to return ownership to the background runner.

### The diff viewer (difit)

The job panel includes a **Run difit** button to inspect any changes or scratch files in the job's worktree:

- Starts a local `difit` server inside the job's worktree:
  ```sh
  npx difit . <base_branch> --no-open --port <port> --host 127.0.0.1 --keep-alive
  ```
- Always passes `--host 127.0.0.1` and sets `HOST=127.0.0.1` in the environment so the server binds strictly to loopback (`127.0.0.1`) on a dynamically allocated port.
- Displays an **Open** link (`http://127.0.0.1:<port>/`) and a **Stop** button.
- Difit is automatically stopped when the job completes, is cancelled, or is taken over.

### The Agents page

The static dashboard sidebar includes an **Agents** link beside **Architecture** (`/agents`):

- Lists all issue agent jobs tracked on this machine across projects.
- Displays state (`Queued`, `Analyzing`, `Stopping`, `Yours`, `Diagnosis ready`, `Ended`, `Stopped`, `Failed`, `Detached`), project, card, issue title, agent, session, worktree, branch, machine, and quick links to open the card or read the diagnosis.
- Filterable by project via `/agents.json?target=<id>`.

#### Staleness

- A queued or running job whose `last_activity_at` is more than 10 minutes old shows **no activity for N min** under its state, here and in the job panel. The panel also shows when the job was last checked (`observed_at`).
- When the signals service has not checked issue agents for more than 2 minutes, both this page and the board show a banner saying so: job states may be out of date until `harness-signals` runs again. A service that has never checked raises the banner only while some job is queued or running.

#### Job states

| State | Meaning |
| --- | --- |
| `Queued` | Waiting for a free runner slot, bounded by global and daily caps. |
| `Analyzing` | Agent is actively running in the background or in a Herdr tab. |
| `Stopping` | A Stop or Take over is in progress: a Herdr agent is being interrupted (`Esc`) and awaited until idle, or a background agent is finishing its current step before it is stopped. |
| `Yours` | You took over the job and its agent may still be working. The runner never signals, stops, closes or resumes it until you hand it back, but it keeps observing it (see "Observation" below). |
| `Diagnosis ready` | Investigation completed; the structured report is displayed on the issue. This includes a job you took over whose session went on to emit a valid diagnosis. |
| `Ended` | The agent is over without a diagnosis: its Herdr tab or process is gone, or it sat idle with a finished reply and no session activity for 10 minutes. The job keeps its owner, and its last event says how the end was seen. Analyze again to start over. |
| `Stopped` | The job was stopped by user request; session and worktree are preserved. |
| `Failed` | The job failed, timed out, or exited with an error. |
| `Detached` | The harness could not verify the process identity (or could not halt a busy Herdr agent), so it signalled nothing. The job keeps its process records; `jobs cleanup` refuses while one may still be running, and Analyze again needs that cleanup first. |

When a job is both yours and finished, the chip shows the outcome (`Diagnosis ready` or `Ended`) rather than `Yours`; `Stopping` wins over both while a stop or take over is in progress.

#### Observation

Every runner tick looks at every running job, whoever owns it, and the service runs the same pass once when it starts, so a restart never strands a job. The service ticks the job runner every 30 seconds, independently of `poll_seconds`.

For each job it reads:

- **Herdr presence:** `herdr agent get <name>` for the agent and its `agent_status`. When Herdr no longer knows the agent, `herdr pane process-info` tells whether the tab is gone or the pane no longer runs the agent.
- **Process identity:** for a background agent, the recorded pid checked against its start time, executable and process group. A pid that cannot be verified is never treated as the agent.
- **Session activity:** the modification time of the agent's transcript (`~/.claude/projects/<bucket>/<session>.jsonl`, or the Codex rollout file), recorded as `last_activity_at`. Every check also records `observed_at`.

A job owned by the runner is still supervised as before; observation only refreshes those two fields. For a job you took over, observation is strictly read-only: it never signals, stops, closes a tab or resumes it. It only changes the status, and only once the agent is over:

| Seen | Result |
| --- | --- |
| The transcript holds a valid diagnosis, and the agent is gone, or idle with its reply finished | `diagnosed`; the report is rendered as for any other job. |
| The agent is gone (tab closed, pane no longer runs the agent, process exited), with no diagnosis | `ended` |
| The agent is idle in Herdr with a finished reply, or no agent process runs at all, and the session has been quiet for 10 minutes | `ended` |
| A recorded process whose identity cannot be verified | `detached`, signalling nothing |
| Herdr cannot answer, or the agent is still working | Unchanged |

The owner stays `you` in each case, and an event records how the end was detected. A runner job whose supervisor died is recovered as before, except that a valid diagnosis in its transcript makes it `diagnosed` rather than `failed`.

### The diagnosis

Once analysis completes, the job chip transitions to `Diagnosis ready` and renders a structured report on the issue drawer with six standard sections:

1. **Likely cause:** Direct explanation of why the failure occurred.
2. **Code involved:** Specific source files, classes, methods, and line references.
3. **Reproduce:** Minimal steps, inputs, or conditions required to trigger the bug.
4. **Suggested fix:** Recommended code changes and necessary automated test coverage.
5. **Blast radius:** Number of users affected, recurring frequency, and downstream impact.
6. **Confidence:** High, medium, or low assessment of the finding.

---

## Setup and configuration

Issue agents run under `services/harness-signals`. Configuration is defined per machine in `~/.config/harness/signals.json` (or `$XDG_CONFIG_HOME/harness/signals.json`) under the top-level `issue_agents` key. All settings are optional:

```jsonc
{
  "poll_seconds": 60,
  "listen": "127.0.0.1:18090",
  "issue_agents": {
    "agent": "claude",               // "claude" | "codex" (default: claude if installed, else codex)
    "global_cap": 2,                 // maximum concurrent jobs across all targets on this machine (1..4, default 2)
    "daily_cap": 5,                  // maximum started jobs per project per UTC day (1..20, default 5)
    "timeout_seconds": 2700,         // per-job execution timeout in seconds (60..7200, default 2700 / 45 min)
    "takeover_wait_seconds": 30,     // how long Take over waits for a background agent to finish its step (5..120, default 30)
    "ssh": "user@host",              // optional SSH destination (user@host or host) for remote takeover commands
    "herdr": {
      "enabled": false,              // true to launch interactive tabs via Herdr; false for background
      "workspace": "Dollarwise"      // Herdr workspace identifier or label (required when enabled is true)
    }
  },
  "projects": [
    {
      "name": "dollarwise-api",
      "target": "DollarWise-Prototype-5173076eded4",
      "environment": "Production",
      "providers": [
        {
          "type": "nightwatch",
          "application": "Dollarwise",
          "credentials_file": "~/.config/harness/signals/nightwatch.oauth"
        }
      ]
    }
  ]
}
```

### Settings reference

| Setting | Type | Range / Default | Description |
| --- | --- | --- | --- |
| `issue_agents.agent` | string | `claude` (if installed) | Agent CLI to launch: `claude` or `codex`. Default claude when installed, otherwise codex. Fails queued jobs if the binary is missing. |
| `issue_agents.global_cap` | integer | `1..4` (default `2`) | Maximum concurrent issue jobs running simultaneously across all projects on this machine. |
| `issue_agents.daily_cap` | integer | `1..20` (default `5`) | Maximum jobs started per project per UTC day. |
| `issue_agents.timeout_seconds` | integer | `60..7200` (default `2700`) | Process timeout in seconds (45 minutes). Terminated and marked failed if exceeded; for a Herdr job, the runner interrupts the agent (`Esc`), waits for it to become idle within the grace period, closes the tab if it does not stop, and only then fails the job. |
| `issue_agents.takeover_wait_seconds` | integer | `5..120` (default `30`) | How long Take over waits for a background agent to finish its current step before stopping it anyway. |
| `issue_agents.ssh` | string | null | Optional SSH destination (e.g. `user@host` or hostname) shown in takeover resume commands. |
| `issue_agents.herdr.enabled` | boolean | `false` | When `true`, creates an interactive Herdr tab for each job. When `false` or absent, jobs run in the background. |
| `issue_agents.herdr.workspace` | string | null | Herdr workspace label or ID where job tabs are created. Required when `herdr.enabled` is `true`. |

### CLI commands

All commands run through `harness-signals jobs` or the shell wrapper `scripts/signals-service.sh jobs ...`:

```sh
harness-signals jobs list [--target ID]
harness-signals jobs show --target ID --job FINGERPRINT
harness-signals jobs analyze --target ID --issue KEY --card CARD [--reanalyze]
harness-signals jobs stop --target ID --job FINGERPRINT
harness-signals jobs takeover --target ID --job FINGERPRINT
harness-signals jobs handback --target ID --job FINGERPRINT
harness-signals jobs cleanup --target ID --job FINGERPRINT [--force-detached]
harness-signals jobs difit start|stop --target ID --job FINGERPRINT
harness-signals jobs run [--once]
harness-signals jobs supervise --target ID --job FINGERPRINT
```

| Command | What it does |
| --- | --- |
| `scripts/signals-service.sh check` | Validates `signals.json`, target paths, secrets, agent installation, and Herdr status. |
| `harness-signals jobs list [--target ID]` | Lists jobs on this machine, optionally filtered by target project, plus `runner`: `{ticked_at, pid}` of the job runner's last tick, or `null` if it has never ticked. |
| `harness-signals jobs show --target ID --job FP` | Displays one job's JSON record, diagnosis text, and diff viewer status. |
| `harness-signals jobs analyze --target ID --issue KEY --card CARD [--reanalyze]` | Queues an Analyze job for an issue on a card. Existing jobs return as-is unless `--reanalyze` is passed. |
| `harness-signals jobs stop --target ID --job FP` | Signals a queued or running job to stop; preserves worktree, session, and diagnosis. |
| `harness-signals jobs takeover --target ID --job FP` | Sets `owner: you`. A background agent stops after its current step (or immediately once `takeover_wait_seconds` runs out); a Herdr agent keeps running. Resume commands are returned once it has stopped. |
| `harness-signals jobs handback --target ID --job FP` | Returns ownership to the runner and resumes process supervision. |
| `harness-signals jobs cleanup --target ID --job FP [--force-detached]` | Removes the job's worktree and branch once all processes have stopped. For a detached job it proceeds only when every recorded process is confirmed gone; after checking yourself that the agent has ended, add `--force-detached`. It never signals anything, and a process verified as still running is refused even with the flag. |
| `harness-signals jobs difit start\|stop --target ID --job FP` | Starts or stops the local `difit` diff viewer server. |
| `harness-signals jobs run [--once]` | Observes running jobs, then starts queued jobs across tracked targets within configured caps, and records the tick. Runs continuously or for one cycle with `--once`. |
| `harness-signals jobs supervise --target ID --job FP` | Internal runner process supervision loop. |

#### Exit codes

The CLI outputs JSON on stdout and reports failures with structured error JSON and standardized exit codes:

- `0`: Success.
- `1`: Unexpected or internal error.
- `3`: Not found (target, job, card, or issue).
- `4`: Conflict (e.g. process or diff viewer still active).
- `5`: Invalid argument or configuration.
- `6`: Quota reached (daily cap exceeded for the target project).

### Test mode

For unit and integration testing without invoking external agent CLIs or altering production files, set:

```sh
HARNESS_ISSUE_AGENTS_TEST_MODE=1
```

In test mode, the runner accepts test argv overrides (`HARNESS_ISSUE_AGENTS_CLAUDE_ARGV`, `HARNESS_ISSUE_AGENTS_CODEX_ARGV`, `HARNESS_ISSUE_AGENTS_HERDR_ARGV`, `HARNESS_ISSUE_AGENTS_DIFIT_ARGV`) and simulates agent and diff viewer lifecycles using deterministic mocks. `HARNESS_ISSUE_AGENTS_IDLE_END_MS` shortens the 10-minute quiet window after which observation ends an idle job (60 seconds by default in test mode).

---

## How it works (optional detail)

```
Problem clicked (Analyze) ──► Fingerprint (innermost in-app frame)
                                     │
                                     ▼
                        Job queued in signals DB
                                     │
       harness-signals background runner (periodically or via tick)
                                     │
                    ┌────────────────┴────────────────┐
                    ▼                                 ▼
         Herdr launcher (tab)              Background launcher (process)
         herdr tab create & agent start    claude -p / codex exec
                    │                                 │
                    └────────────────┬────────────────┘
                                     ▼
                     Isolated worktree & branch nw/<fp>
                     Strict empty MCP config (no tools)
                     Bounded 24 KiB scrubbed context packet
                                     │
                                     ▼
                         Agent investigates code
                                     │
                                     ▼
                Extract JSON between marker delimiters:
          <<<ISSUE_DIAGNOSIS_JSON>>> ... <<<END_ISSUE_DIAGNOSIS_JSON>>>
                                     │
                                     ▼
              Render Markdown diagnosis: analyses/<fp>.md
              Status: diagnosed · Displayed on board drawer
```

### Storage and data contract

All issue agent data is managed by `services/harness-signals` and stored strictly within the harness database directory for each target:

- `<db>/targets/<id>/signals/jobs/<fingerprint>.json`: Job state v1 records.
- `<db>/targets/<id>/signals/analyses/<fingerprint>.md`: Formatted Markdown diagnosis reports.
- `<db>/targets/<id>/signals/jobs/<fingerprint>.log`: Execution and runner logs.
- `<db>/targets/<id>/signals/jobs/<fingerprint>.pid`: Process ID of active background agent.
- `<db>/targets/<id>/signals/jobs/quota.json`: Daily started job counters keyed by UTC date (`YYYY-MM-DD`).
- `<db>/targets/<id>/signals/jobs/runner.lock`: Lock guarding job queue execution.
- `<db>/targets/<id>/signals/worktrees/<fingerprint>`: Dedicated git worktree for the job.
- `<db>/issue-agents/runner.json`: `{ticked_at, pid}` of the job runner's last tick on this machine, reported by `jobs list` as `runner`.

Jobs are preserved across signals configuration pruning to prevent accidental deletion of diagnostic history.

#### Job JSON schema (v1)

```jsonc
{
  "version": 1,
  "fingerprint": "9f3c1a2b8e01",
  "target_id": "DollarWise-Prototype-5173076eded4",
  "card_id": "Billing",
  "issue_key": "517",
  "issue_title": "TypeError: Subscription::renew(): Argument #1 ($plan) must be of type Plan, null given",
  "issue_url": "https://nightwatch.laravel.com/...",
  "root_frame": {
    "path": "app/Billing/Subscription.php",
    "line": 142
  },
  "mode": "analyze",
  "status": "running",                  // queued | running | diagnosed | ended | stopped | failed | detached
  "stopping": null,                     // set while a Stop or Take over is in progress, otherwise null/omitted
  "run": "3fa0c2d19b7e4a61",            // new on every start; a supervisor writes only to the run it was started for
  "takeover_wait_seconds": 30,          // reported by jobs show
  "unresolved_processes": [],           // detached jobs: role, pid and start time of processes that may still run
  "owner": "runner",                   // runner | you
  "created_at": "2026-10-25T15:30:12Z",
  "updated_at": "2026-10-25T15:33:40Z",
  "started_at": "2026-10-25T15:30:15Z",
  "finished_at": null,
  "last_activity_at": "2026-10-25T15:33:40Z", // last write to the agent's session transcript
  "observed_at": "2026-10-25T15:34:00Z",      // last time a runner tick checked this job; null before the first
  "agent": "claude",
  "session_id": "3b1e6f0c-8d2a-4c57-9a41-72e5d0b9c8aa",
  "worktree": {
    "path": "/path/to/.harness-db/targets/DollarWise-.../signals/worktrees/9f3c1a2b8e01",
    "branch": "nw/9f3c1a2b8e01",
    "base_branch": "master",
    "base_commit": "e3b3ba9f1234567890abcdef1234567890abcdef"
  },
  "machine": "macbook-pro",
  "launcher": "herdr",                 // herdr | background
  "herdr_tab": {
    "tab_id": "tab-12849",
    "pane_id": "pane-39210"
  },
  "pid": 48211,
  "diagnosis_path": "analyses/9f3c1a2b8e01.md",
  "difit": {
    "port": 4966,
    "pid": 48390,
    "started_at": "2026-10-25T15:31:00Z"
  },
  "error": null,
  "events": [
    {
      "at": "2026-10-25T15:30:15Z",
      "type": "launch",
      "summary": "Agent launched in Herdr tab issue-9f3c1a2b"
    }
  ]
}
```

### Fingerprinting and deduplication

The job fingerprint identifies the distinct root cause of an error:

1. Takes the innermost stack frame marked `in_app` with a repository-relative path and line number.
2. Normalizes path separators to `/`, resolves relative segments, and applies target path mappings.
3. Computes the SHA-256 hash of `target_id + "\n" + card_id + "\n" + normalized_path + "\n" + line`.
4. If an issue lacks an in-app frame, Analyze returns HTTP 422 (CLI exit code 5) and creates no job.
5. If a job for that fingerprint already exists, it is returned without re-running. Finished or stopped jobs require an explicit `reanalyze=true` parameter to re-queue.

### Worktree lifecycle

- Each job creates an isolated git worktree under `<db>/targets/<id>/signals/worktrees/<fingerprint>` on a new branch named `nw/<first-12-hex-fingerprint>`.
- The base branch is derived from the board's `default_branch` (falling back to remote `HEAD` or current branch), and `worktree.base_commit` records the commit the branch was branched from.
- The worktree remains intact after completion, stoppage, or failure so you can inspect or resume the session.
- An existing `nw/<first-12-hex-fingerprint>` branch is reused only if it is not checked out elsewhere and its tip is either the recorded `base_commit` or descends from it through commits that exist on no other branch, remote, or tag (meaning they were made exclusively by this job in its worktree). Otherwise the runner refuses to reuse it: run `harness-signals jobs cleanup --target <target-id> --job <fingerprint>`, then trigger Analyze again.
- An explicit cleanup command (`harness-signals jobs cleanup --target <target-id> --job <fingerprint>`) removes the worktree and deletes the branch only when all child processes have stopped and the branch has no commits unique from base (preserving branches with new commits for manual review or deletion).

### Read-only guarantees and safety

The agent is strictly constrained to prevent accidental modifications:

1. **Empty MCP configuration:** In phase 1, no external MCP tools (including Nightwatch read/write tools) are passed to the agent.
2. **Tool allowlists:**
   - **Claude Code:** Invoked with `--tools Read,Grep,Glob`, `--permission-mode dontAsk` (denying actions that require approval), `--setting-sources ""` (disabling local or user settings), and `--strict-mcp-config`.
   - **Codex:** Invoked with `--sandbox read-only` and `--ask-for-approval never`.
3. **No repository modifications:** The agent is given explicit system instructions to produce only a diagnosis. Worktree contents are checked before and after execution to guarantee tree-hash integrity.

### Context packet and diagnosis format

The runner extracts relevant information from `harness-signals` and builds a bounded context packet (maximum 24 KiB) written to a temporary file:

- Issue title, severity, occurrence count, first seen, last seen, and issue tracker URL.
- Card title, summary, responsibility, and owned paths/files/tests.
- Innermost `in_app` frames (up to 30 frames, paths and lines only).
- All UUIDs and email addresses are sanitized using signals redaction filters.
- Raw logs, query parameters, HTTP headers, request bodies, and credentials are completely excluded.

The agent concludes its investigation by emitting a JSON payload enclosed within literal markers:

```
<<<ISSUE_DIAGNOSIS_JSON>>>
{
  "likely_cause": "Subscription::plan() returns null for subscribers whose plans were archived.",
  "code_involved": "app/Billing/Subscription.php:142, app/Billing/Plan.php:31",
  "reproduce": "Archive an active plan, then trigger Subscription::renew() for a subscriber on that plan.",
  "suggested_fix": "Include archived plans in subscription renewal queries and add test coverage.",
  "blast_radius": "Affects 21 users in 24 hours; all renewals on archived plans fail.",
  "confidence": "high"
}
<<<END_ISSUE_DIAGNOSIS_JSON>>>
```

The runner parses the JSON, validates that all six fields are present and bounded (maximum 4,000 characters each, 12 KiB total), and renders the report to `<db>/targets/<id>/signals/analyses/<fingerprint>.md`.

### Dashboard API endpoints

The `services/harness-audit` dashboard exposes REST endpoints for issue agents:

| Method & Route | Description |
| --- | --- |
| `GET /agents` | HTML page rendering all issue jobs on this machine. |
| `GET /agents.json?target=<id>` | JSON summary `{jobs: [...], generated_at, runner}` for this machine, optionally filtered by project target ID. |
| `GET /projects/{id}/agents/{fingerprint}.json` | Detail endpoint returning `{job, diagnosis, difit_running}` with `Cache-Control: no-store`. |
| `POST /projects/{id}/agents` | Queues an Analyze job (`csrf_token`, `issue_key`, `card_id`, optional `reanalyze`). |
| `POST /projects/{id}/agents/{fingerprint}/stop` | Stops an active or queued job. |
| `POST /projects/{id}/agents/{fingerprint}/takeover` | Sets `owner: you`. A background agent stops after its current step; a Herdr agent is released. Resume commands follow once it has stopped. |
| `POST /projects/{id}/agents/{fingerprint}/handback` | Re-attaches runner supervision or queues continuation. |
| `POST /projects/{id}/agents/{fingerprint}/difit/start` | Starts local `difit` on an available loopback port. |
| `POST /projects/{id}/agents/{fingerprint}/difit/stop` | Terminates the running `difit` instance. |
| `GET /projects/{id}/agents/{fingerprint}/difit` | Validates process liveness and redirects to `http://127.0.0.1:<port>/`. |

All state-changing `POST` requests require CSRF validation.
