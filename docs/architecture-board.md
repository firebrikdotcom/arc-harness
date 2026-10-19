# Architecture board in the dashboard: design

Status: built 19 October 2026 (`scripts/board.py`, `tools/architecture-board/`, the `SessionStart` trigger in `scripts/hooks/auto-init.sh`, and the dashboard's Architecture pages). `board export` is not built yet. Operating notes: [setup.md](setup.md#architecture-boards).

Goal: for any software project you work on, the harness dashboard shows a whiteboard of that project's components. The board has fixed cards in labelled frames, typed connection lines, pan and zoom, and a details panel listing responsibilities, connections, source files and tests. The harness makes and refreshes the board by itself, in the background, and **never adds or changes anything in the project's repository**. The reference look and behaviour is the board built by hand for Firebrik Terminal (`docs/architecture-explorer.html` in that app's repository; its data is the fixture `tools/architecture-board/fixtures/firebrik-terminal.board.json`).

---

## 1. What exists today (verified)

| Fact | Evidence |
|---|---|
| Claude Code and Codex run one harness hook at session start. It registers the project as a target, runs the bootstrap, prints a short brief and hands off to the task-entry route. It always exits 0. `HARNESS_AUTO_INIT=0` disables it. | `scripts/hooks/auto-init.sh`, bound by `scripts/session_hook.py` |
| Every target the harness touches is registered with its root path. | `.harness-db/targets/<id>/target.state` (`TARGET_ROOT=`, `LAST_SEEN_AT=`) |
| Each target already has a generated project map (top-level dirs, file counts, checks). | `.harness-db/targets/<id>/map.md`, written from `scripts/harness:655` |
| The harness can hash a project's files as they are on disk, with or without git. | `scripts/tree-hash.sh` |
| External agent profiles run as exact argv, in bounded process groups with a timeout. | `docs/orchestration.md` (`dispatch.profiles`, `dispatch.timeout_seconds`); `scripts/role_dispatch.py` |
| The dashboard is a Rust actix-web service. It renders server-side with Tera and embeds its assets in the binary. | `services/harness-audit/Cargo.toml`; `src/ui.rs:24-39` |
| Its pages are `/`, `/orchestration`, `/workflow` and `/events`, and the sidebar nav is a static list. | `src/ui.rs:142-160` (`NAVIGATION`) |
| It listens on `127.0.0.1:18080` only. | `services/harness-audit/.env.example`; `ops/harness-audit.service` |
| The audit store holds compact measurements and hashed evidence only. | `docs/architecture.md:57` |
| Workflow sessions carry their `cwd`, and the dashboard derives the project from it. | `src/workflow.rs:667-669` (`project(cwd)`) |
| The reference board is one self-contained HTML file: data (frames, nodes, edges, flows) plus about 400 lines of renderer, with no libraries. | `android-terminal/docs/architecture-explorer.html` |

What follows from these facts: the session-start hook is the right trigger, the target directory in the harness DB is the right home for the board, and the audit store stays untouched.

---

## 2. How it works, in one pass

```
session starts in a project
        │
        ▼
auto-init.sh ──► harness board check            (deterministic, no AI, < 1 s)
                    │
                    ├── not a software project / opted out ─► nothing
                    ├── board current ──────────────────────► nothing
                    └── no board, or anything drifted
                           │
                           ▼
                 harness board map --background  (detached; the session never waits)
                           │   lock: one mapper per project
                           ▼
                 mapper agent, read-only tools, returns JSON on stdout
                           │
                           ▼
                 harness validates it ──► .harness-db/targets/<id>/board/board.json
                                                         │
dashboard sidebar: "Architecture · <project>" ◄─────────┘  (reads it at view time)
```

Five parts:

1. **A data contract.** `board.json` lives only in the harness DB (§3).
2. **A deterministic check** that decides whether to map (§4).
3. **A background mapper** that the harness launches itself. It is not a skill and not something the agent must remember (§5).
4. **One renderer**, extracted from the Firebrik board and embedded in the dashboard (§8).
5. **A dashboard view** with a sidebar entry for the current project, plus drift and live-work highlights (§7).

---

## 3. Data contract: `board.json` (in the harness DB)

Location: `.harness-db/targets/<id>/board/`. It holds:

- `board.json`: the board itself.
- `manifest.json`: the path and content hash of every file the last mapping saw (hashes only, no contents). Drift checks use it.
- `state.json`: `mapping | refreshing | ready | failed`, with timestamps, the last error, warnings and the run's cost and duration.
- `check.json`: the last check's result (action, reason, drift counts), so the dashboard sidebar never has to hash files.
- `mapper.lock`: holds the mapper's pid while it runs; a lock whose process is gone is stale and ignored.
- `mapper.log` and `last-answer.txt`: the mapper's log lines and its last raw answer, for diagnosing a failed run.

Nothing is written under `TARGET_ROOT`.

```jsonc
{
  "version": 1,
  "title": "Firebrik Terminal",
  "repo_url": "https://github.com/firebrikdotcom/firebrik-terminal",   // optional, read from git remote; enables "open on GitHub"
  "default_branch": "main",
  "mapped_at": "2026-10-19T11:40:00Z",
  "mapped_tree_hash": "…",               // tree-hash.sh at mapping time
  "edge_kinds": {"call": {"label": "In-app call", "style": "solid"}},
  "frames": [{"id": "app", "title": "App module", "tag": ":app · Android services", "tone": "blue",
              "col": 1, "row": 0, "cols": 2, "rows": 3, "external": false}],
  "nodes": [{"id": "session", "frame": "app", "col": 2, "row": 0, "icon": "plug",
             "title": "Session + SSH", "sub": "connect · host key · PTY · SFTP",
             "summary": "…", "responsibility": "…",
             "files": [{"path": "app/src/main/java/…/HerdrSession.kt", "note": "Connection lifecycle"}],
             "tests": ["app/src/test/java/…/HerdrSessionEndToEndTest.kt"],
             "owns": ["app/src/main/java/com/firebrik/terminal/ssh/"]}],
  "edges": [{"from": "session", "to": "shell", "label": "SSH exec + PTY", "kind": "remote"}],
  "flows": [{"id": "connect", "title": "Connect to a server", "steps": [["user","compose"]], "text": ["…"]}]
}
```

Rules:

- **The layout is data.** Positions are grid cells (`col`/`row`), never pixels, so the board stays fixed between refreshes. The renderer owns spacing, edge routing and label placement.
- **`owns` maps every source file to a card.** Drift and the live highlights both depend on it.
- **Tones are names, not colours.** The renderer maps them to light and dark palettes.
- **Text is untrusted.** An agent writes it, so the renderer escapes every string.

---

## 4. When to map: `harness board check`

`board check` is plain code, needs no AI and finishes in well under a second. The session-start hook runs it every time and acts on the result:

| Result | Action |
|---|---|
| Not a software project (no source files or build manifest in `map.md`), `.harness-no-upload` present, or `board.enabled=false` | Nothing |
| A mapper is already running (`mapper.lock` held by a live process) | Nothing |
| No board yet | Full mapping |
| `tree-hash.sh` equals `mapped_tree_hash` | Nothing: the board is current |
| Anything differs | **Drifted**, so refresh (below) |

**Drift means any change.** There is no threshold. Comparing the current files with `manifest.json` gives three lists:

- **Changed files:** the cards whose `owns` covers them get re-described.
- **New files no card owns:** the mapper places them, on an existing card or a new one.
- **Deleted files:** they are removed from their cards, and a card that is left empty is removed.

A refresh is incremental. The mapper gets the current board plus only the affected cards and files, and it must keep every other card, position and flow as it is. Small edits cost little, and the board does not reshuffle.

Uncommitted edits count, because `tree-hash.sh` sees the files on disk. While you work, the live highlights (§7) show them at once. The next session start then folds them into the board.

---

## 5. The mapper: a harness-launched worker, not a skill

A skill runs only when an agent decides to load it, and this must happen every time without anyone asking. So the harness launches the mapper itself:

- **Trigger:** `auto-init.sh` starts `scripts/board.py check --target <cwd> --spawn --quiet` detached, so even the file hashing never delays the session. When the check says to map, it starts `board.py map` in a new session with output to `board/mapper.log`. The hook keeps its "always exits 0" contract and prints nothing for the board; the dashboard shows the state.
- **Who runs it:** `claude -p --model sonnet --tools Read,Grep,Glob --setting-sources "" --strict-mcp-config --no-session-persistence --output-format json` in its own process group, killed after `HARNESS_BOARD_TIMEOUT` (default 1200 s). `HARNESS_BOARD_MODEL` changes the model and `HARNESS_BOARD_MAPPER_CMD` (a JSON argv) replaces the command, for example with `codex exec` in its read-only sandbox. (Routing it through a `dispatch.profiles` entry is a possible later change.)
- **What it gets:** a fixed prompt kept in harness-template (`tools/architecture-board/mapper-prompt.md`), plus `map.md`, the current board (for a refresh), the drift lists, and the schema. The prompt asks for the same quality as the Firebrik board: 10 to 25 cards in a few frames, a responsibility for each card, real files and tests, typed edges, and two or three main flows.
- **How it cannot touch the repo:**
  1. The agent gets read-only tools or a read-only sandbox.
  2. It returns the board as JSON on stdout and writes no files.
  3. The harness validates the JSON against the schema, then checks that every path exists, every id is unique, no two cards share a cell, every card sits inside its frame, and every edge and flow names real cards. Only then does it write to the DB.
  4. The harness records `tree-hash.sh` at launch and stores that value as `mapped_tree_hash`. If you edit files while the mapper runs, the next check sees drift and refreshes again. (The read-only sandbox is the guarantee; the harness cannot tell your edits from the agent's, so it does not try.)
- **No recursion:** the mapper is itself an agent session, so it starts with `HARNESS_AUTO_INIT=0` and `HARNESS_BOARD_MAPPER=1`, and loads no user or project settings (`--setting-sources ""`), so no session-start hook fires inside it and it can never spawn another mapper.
- **Failure:** `state.json` records `failed` with the reason. The dashboard shows it, and the next session start tries again. A broken mapping never blocks the session.
- **Cost:** a full mapping is one agent run per project. A refresh sends only the affected cards. Every run is recorded as a normal dispatch measurement (profile, duration, exit, tokens), never with its prompt or output.

---

## 6. Commands

| Command | What it does | Deterministic? |
|---|---|---|
| `harness board check [--spawn]` | Runs the decision in §4 and prints the drift lists. With `--spawn` it starts the mapper when needed. | Yes |
| `harness board map [--full]` | Runs the mapper in the foreground, as in §5. The hook uses its background form. `--full` ignores the existing board. | No (agent) |
| `harness board status` | Shows state, a fresh check and the mapper log tail. | Yes |
| `harness board import --board FILE` | Installs a hand-made board after the same validation (`tools/architecture-board/import-explorer.mjs` converts an explorer page). | Yes |
| `harness board list --json`, `show`, `state --json`, `overlay --json` | What the dashboard reads: the sidebar rows, the board, the page status, and the live highlights. | Yes |
| `harness board export --out PATH` | *Not built yet.* Writes a standalone HTML copy (renderer and data inlined) to a path **you** choose, for sharing. It is never run automatically. | Yes |

---

## 7. Dashboard view

**Sidebar:** beneath the static `NAVIGATION` items, an **Architecture** entry for every project that has a board or a mapping state, then the four most recently active other projects (registered targets by `LAST_SEEN_AT`; home directories and projects the last check found non-software or opted out are left out). Each entry shows its state: `mapping…`, `ready`, `drifted`, `refreshing…` (the old board stays visible), `failed`, or `not mapped`. A link from the Workflow session view to its project's board is not built yet.

**Routes** (in `services/harness-audit`, following the existing page pattern). The service holds no board logic: it runs `python3 scripts/board.py --db-root <db> list|show|state|overlay` off the request thread and passes the JSON through, caching `list` for a few seconds because every page's sidebar shows it. `HARNESS_BOARD_SCRIPT` and `HARNESS_DB_ROOT` override the script and database paths.

- `GET /projects/{target_id}/board`: the board page. The Tera template holds only the shell; `board.js` is embedded with `include_str!` the same way `styles.css` is.
- `GET /projects/{target_id}/board.json`: reads `.harness-db/targets/<id>/board/board.json` and `state.json`.
- `GET /projects/{target_id}/overlay.json`: the live highlights (below).

**Live highlights ("what am I touching")** are computed per request and never stored:

- Cards whose `owns` covers a file in `git status --porcelain` in `TARGET_ROOT` get an **edited now** ring.
- Cards whose `owns` covers the active harness run's contract `deliverables` get an **in scope** marker (`db/runs/<run>/task.json`).
- Cards changed since `mapped_tree_hash` get a **refreshing** badge while the mapper runs.
- The page polls every 5 s, and only while the tab is visible.

**Safety:**

- The service reads `TARGET_ROOT` only for `git status`, and resolves real paths so nothing escapes a registered root.
- It serves only on `127.0.0.1`. File links open on the forge (`repo_url`) or copy the path. The dashboard never serves source file contents.
- It sets a strict CSP on the page: no inline script except the embedded renderer, and no remote origins.

---

## 8. Renderer extraction (from the Firebrik board)

What moves into `tools/architecture-board/board.js` unchanged:

- The grid layout, edge routing (port spreading, same-row/column arcs) and label collision avoidance
- Pan, zoom and pinch; frame jump; search; flows; line-kind legend; light and dark themes; `#node` deep links
- The details panel

What changes:

- It reads data from `fetch(board.json)` (dashboard) or `window.BOARD` (export) instead of constants.
- `REPO` comes from `repo_url` and `default_branch`. Without them, only Copy is shown.
- The icon set becomes a fixed named list with a fallback glyph.
- It gains overlay hooks: `setOverlay({edited:[ids], inScope:[ids], refreshing:[ids]})`.
- A small Playwright spec (`tests/board.spec`) runs the existing browser checks against a fixture: no console errors, select, flow, search, pan, zoom, and phone width.

**Firebrik Terminal's role:** its hand-built board is converted, once, into the test fixture and into the example of a good board in the mapper prompt. The file in the Firebrik repository stays as it is; the harness only reads it.

---

## 9. Order, and what can be dropped

1. **Schema, renderer extraction and the Firebrik fixture.**
2. **`board check`** (software-project detection, manifest, drift lists).
3. **Mapper** (`board map`, the `board_mapper` profile with read-only tools, the prompt, validation).
4. **Session-start trigger** (`check --spawn` in `auto-init.sh`, lock, state).
5. **Dashboard board page and sidebar entries** (read-only).
6. **Live highlights** (git status, run scope, refreshing).
7. **`board export`.**

Steps 1 to 6 are built. Step 7 (`board export`) can come later without changing the others.

---

## 10. Out of scope, said explicitly

- Any file in the project's repository, whether board, config, hook or generated HTML.
- Auto-generated dependency graphs from imports. They are accurate but unreadable at this size, and that is what the board replaces.
- Live runtime telemetry (process health, traffic). The board is static architecture plus *your* working state.
- Dragging cards in the dashboard. The layout stays fixed between refreshes.
- Storing board content in the audit event store.

---

## 11. Decisions

Settled (19 October 2026):

1. **Storage:** the harness DB only; nothing is added to or changed in the project repository.
2. **Creation:** automatic. The session-start hook launches a background mapper for software projects.
3. **Refresh trigger:** any drift, with no threshold. Refreshes are incremental.
4. **Mechanism:** the harness launches the mapper itself, not a skill.
5. **Dashboard:** a sidebar entry per project, showing the mapping state.

Still open:

1. **Mapper profile.** Shipped with Sonnet through `claude -p` (about $0.28 and two minutes for a 200-file project); `HARNESS_BOARD_MODEL` and `HARNESS_BOARD_MAPPER_CMD` change it.
2. **Live highlight sources.** *Recommend* git status plus run contract deliverables. Possible later addition: files the agent edited in the current session, which would need the workflow audit to record edited paths. Today it deliberately does not.
