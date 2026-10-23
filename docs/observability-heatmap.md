# Error heat on the architecture board

**What it is.** `harness-signals` is a background service that reads the errors, exceptions and warnings your projects already report to their observability tools, works out which architecture-board card each one belongs to, and colours those cards on the dashboard. The hotter a card, the more trouble that part of the system is causing right now. Click a card to see its worst issues, with links back to the tool.

**Each machine decides what it tracks.** A machine tracks only the projects listed in its own `~/.config/harness/signals.json` (or `$XDG_CONFIG_HOME/harness/signals.json`). No file means the machine tracks nothing. Nothing is written into the projects, and tokens stay in files you create or that the sign-in writes; the service reads them and never stores or logs them.

**Supported sources.** The service reaches out to each source itself every `poll_seconds` (60 by default).

| Source | How the service gets errors | What you provide |
| --- | --- | --- |
| Laravel Nightwatch | Polls Nightwatch's MCP server, as any MCP client does: open issues, their occurrence counts, users affected and stack traces | A one-time browser sign-in: `scripts/signals-service.sh login nightwatch` |
| Sentry | Polls the issue API | An auth token with read access, in a file |
| Anything else | Runs a command you name; it prints issues as JSON | The command |

Nightwatch can also push signed webhooks. They are optional: with polling configured, a webhook only makes the service poll that project at once, so a new error appears within seconds instead of at the next cycle.

**Not tied to one tool.** Only the way credentials are obtained differs per source: Nightwatch needs the browser sign-in because it offers no API tokens, Sentry takes a token file, and the command provider takes whatever your own program needs (Bugsnag, Datadog, a log file, an in-house tracker). A project can list several providers at once, and mapping to cards, scoring and the Heat layer are the same whatever the source.

**No environment variables or secrets in config.** `signals.json` holds no secrets, only the paths of the files that do. Nothing has to be exported in the shell: the background service finds its configuration in the default location and the secret files through the paths in it.

## What a working setup consists of

Taking one machine that polls one project from Nightwatch as the example:

1. `scripts/signals-service.sh login nightwatch` registered `harness-signals` with Nightwatch, a person approved it in the browser, and the refresh token was saved to `~/.config/harness/signals/nightwatch.oauth` (owner-only).
2. `~/.config/harness/signals.json` names the project (a harness target), the Nightwatch application, the environment, and that credential file.
3. `scripts/signals-service.sh install` installed the background service (launchd `local.harness.signals` on macOS, systemd `harness-signals.service` on Linux), which starts at login.
4. Every `poll_seconds` the service trades the refresh token for a short-lived access token, reads the open issues, and saves the rotated refresh token back to the same file.
5. It writes the results under `.harness-db/targets/<id>/signals/`, and the dashboard's board draws them as the Heat layer. Its log is `~/Library/Logs/harness-signals.log` on macOS, or `journalctl --user -u harness-signals` on Linux.

Another machine needs the same steps of its own: the code, the installed service, its own `signals.json`, and its own sign-in (the credential rotates, so a copy stops working). Two machines tracking the same project both poll, so usually one machine per project is enough.

## Setting up a machine

1. Write `~/.config/harness/signals.json` (example below) naming the projects this machine should watch. A project is a harness target id (`scripts/board.py list` shows them), or a path inside a registered project.
2. For Nightwatch, sign in once on this machine: `scripts/signals-service.sh login nightwatch`. It opens Nightwatch's approval page in your browser and saves a refresh credential to `~/.config/harness/signals/nightwatch.oauth`, readable only by you. On a machine without a browser, open the printed link on any machine, approve, and paste the address the browser lands on (`http://127.0.0.1:…/callback?code=…`) back into the terminal. Sign in on each machine separately: the credential rotates when it is refreshed, so a copy would stop working.
3. For Sentry, save each token in its own file, readable only by you:

   ```sh
   mkdir -p ~/.config/harness/signals && chmod 700 ~/.config/harness/signals
   read -rs t && printf %s "$t" > ~/.config/harness/signals/sentry.token && chmod 600 ~/.config/harness/signals/sentry.token
   ```

4. Check it: `scripts/signals-service.sh check` lists each project, the target it resolved to, whether the board exists, and whether each secret file is readable (never its contents).
5. Install the background service: `scripts/signals-service.sh install` (launchd `local.harness.signals` on macOS, systemd user unit `harness-signals.service` on Linux). It re-reads the configuration every cycle, so later edits need no restart, except a change of `listen`.

```jsonc
{
  "poll_seconds": 60,            // how often pull sources are polled (minimum 10)
  "half_life_minutes": 180,      // an error's weight halves every 3 hours
  "listen": "127.0.0.1:18090",   // webhook receiver; keep it on loopback
  "projects": [
    {
      "name": "dollarwise-api",                       // also the webhook path segment
      "target": "DollarWise-Prototype-5173076eded4",  // or a path inside the project
      "environment": "Production",                    // only errors from this environment count
      "path_map": [{"from": "/var/www/html/", "to": ""}],
      "providers": [
        {"type": "nightwatch", "application": "Dollarwise",
         "credentials_file": "~/.config/harness/signals/nightwatch.oauth"},
        {"type": "sentry", "org": "dollarwise", "project": "api",
         "token_file": "~/.config/harness/signals/sentry.token"},
        {"type": "command", "argv": ["/path/to/print-issues"], "timeout_seconds": 60}
      ]
    }
  ]
}
```

The Nightwatch provider takes `application` (name or id; optional when the account has one application), `limit` (most open issues read per poll, default 50) and, for webhooks, `secret_file`. The project's `environment` is matched against Nightwatch's environment names, ignoring case.

### Optional: Nightwatch webhooks for instant updates

In Nightwatch, open Issues settings › Webhooks, add a webhook pointing at `https://<your public host>/hooks/nightwatch/<project name>`, and save its signing secret to the provider's `secret_file`. The service listens on loopback only, so the URL must reach it through a tunnel you choose (for example Cloudflare Tunnel or Tailscale Funnel) that forwards only `/hooks/nightwatch/`. Deliveries without a valid `Nightwatch-Signature` are refused with 401. Without `credentials_file`, webhooks are the only source: each `issue.opened` or `issue.reopened` counts as one occurrence and the stack is just the throwing file.

### Removing a project or a source

Delete it from `signals.json`. On its next cycle the service drops that source's issues; a project no longer listed loses its stored heat, so its board stops showing it. Cleanup is skipped while the configuration file is missing or unreadable, or while any listed project cannot be found, so a typo or a briefly missing checkout never erases history. `scripts/signals-service.sh prune` runs the same cleanup on demand.

---

## How it works (optional detail)

```
Sentry API ──poll──┐
command ─────run───┼─► normalise ─► map frames to cards ─► score ─► <db>/targets/<id>/signals/heat.json
Nightwatch ──push──┘    (one issue      (path_map, then the         (level × volume × reach,     │
                         shape)          cards' owns/files/tests)     decayed over time)          ▼
                                                              board.py overlay ─► dashboard Heat layer
```

**Storage.** Everything lives under `<db>/targets/<id>/signals/`: `issues.json` (the current issues of every source), `poll.json` (each source's last poll: ok, time, count, error) and `heat.json` (per-card scores). Only short summaries are kept: a one-line title of at most 200 characters with UUIDs and email addresses replaced by `<id>` and `<email>`, level, counts, first and last seen, the link, and file/line stack frames. Messages beyond the title, request data, queries, logs and user identifiers are dropped.

**Nightwatch polling.** Each poll calls only Nightwatch's read tools: `list_applications` and `list_environments` (to resolve the names in the config), `list_issues` (open issues, most recently seen first, up to `limit`), and `get_issue` for issues that are new or have recurred since the last poll, at most 15 per poll. It never calls the tools that comment on or change issues. `get_issue` supplies the stack trace (repository paths, `vendor/` frames marked as not in the app), occurrences in the last 24 hours and users affected. Exceptions count as errors (`FatalError` as fatal); route, job, command and scheduled-task issues, which are threshold alerts, count as warnings. Access tokens are refreshed with the stored refresh token, which is rotated and saved back; if Nightwatch refuses it, the poll records the error and you sign in again.

**Mapping to cards.** For each issue, the frames in your own code (`in_app`) are turned into repository paths: known prefixes such as `file://` and `webpack:///` are stripped, the first matching `path_map` rule is applied, and the project's own root is removed. A card owns a path exactly as `scripts/board.py` decides it (its `owns`, `files` and `tests` entries: equal, directory prefix, or `fnmatch` pattern). When no card owns a path, leading directories are dropped one at a time, so `/srv/releases/42/app/Models/User.php` still finds the card owning `app/Models/User.php`. The cards owning the innermost frame get the issue's full score; cards further up the call path get 30%. An issue no card owns is listed as unmapped, never dropped.

**Scoring.** `level weight × ln(1 + recent events) × (1 + ln(1 + users))`, with weights fatal 8, error 5, warning 1, info 0.2. Recent events decay with `half_life_minutes`: Sentry's hourly buckets are decayed one by one; other sources decay their count by the time since the issue was last seen. A card's heat is its total divided by the hottest card's, so the worst card on each board is 100%. Issues first seen in the last 24 hours are marked new.

**On the board.** The Heat button (shown when the project has heat) tints cards by heat and adds an "errors · N" badge; it is remembered per browser. The overview lists the five hottest cards, the number of unmapped issues, sources whose last poll failed, and a stale warning when `heat.json` is more than 15 minutes old. A card's details gain a Problems list: level, title, events, users, last seen, whether the card raised it or is on its call path, and an Open link.

**Command output.** A JSON array, or `{"issues": [...]}`, of objects with `id` and `title`, and optionally `level` (`fatal|error|warning|info`), `status` (`open|resolved|ignored`), `count`, `users`, `first_seen`, `last_seen` (RFC 3339), `url`, and `frames` (`[{"path": "...", "line": 12, "in_app": true}]`, innermost first). Its output is capped at 8 MB and it is killed after `timeout_seconds`.

**Commands.**

| Command | What it does |
| --- | --- |
| `scripts/signals-service.sh check` | Validates the configuration and shows what this machine tracks. Exits 1 when a target or secret is missing. |
| `scripts/signals-service.sh once` | Polls every project once and prints a summary per project. |
| `scripts/signals-service.sh serve` | The service itself: webhook receiver plus the poll loop. |
| `scripts/signals-service.sh login nightwatch` | One-time browser sign-in for polling Nightwatch. |
| `scripts/signals-service.sh tools nightwatch` | Lists the Nightwatch MCP tools, for diagnosis. |
| `scripts/signals-service.sh prune` | Removes stored heat of projects no longer configured. |
| `scripts/signals-service.sh install`, `status` | Install or inspect the background service. |

`HARNESS_SIGNALS_CONFIG` overrides the configuration file path. The binary also takes `--config FILE`, `--db-root DIR` and, for `login`/`tools`, `--credentials FILE` and `--mcp-url URL`.

**Limits and edge cases.**

- Sentry: one issue-list call per poll (unresolved, seen in the last 24 hours, up to `limit`, default 50), plus the latest event of each new issue, at most 20 per poll; frames are fetched once per issue. Use `base_url` for a regional or self-hosted Sentry.
- A failed poll keeps that source's earlier issues and records the error, which the board shows.
- Webhook bodies over 256 KB are refused. Each project may have one Nightwatch provider. The receiver handles one request at a time, and a captured signed delivery could be replayed; with polling configured, a replay only causes an extra poll.
- A Sentry `base_url` or Nightwatch `mcp_url` must be https; plain http is accepted only for the exact hosts `127.0.0.1`, `localhost` and `::1`.
- The board calls heat stale after 15 minutes without an update, so keep `poll_seconds` well below that.
- Heat is relative: the worst card on a board is always 100%, even on a quiet day. Compare a card's issue counts in its details for absolute volume.
- The service serialises its own writes; running `once` by hand while the service runs can, rarely, lose a webhook that lands at the same moment.
- The service writes only under the harness DB; the dashboard reads the heat through `board.py overlay` and holds no logic of its own.
