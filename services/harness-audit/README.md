# Harness audit service

This is the versioned Arc event-sourced audit sink for the harness. It stores
JEV routing, resolved outcomes, and agent/token measurements as immutable
`AuditEventRecorded` events, then projects them into an `audit_events_view`
read model for summaries and review.

It is intentionally local-only by default:

```sh
cp .env.example .env
cargo run -- migrate
cargo run -- serve
curl http://127.0.0.1:18080/health
```

The harness client treats the service as best-effort telemetry. If it is down,
the agent task continues and the local TypeSafe logs remain authoritative for
the route call itself.

Browser pages (Arc UI host, same read model as the API):

- `GET /` — audit workbench: a daily accuracy line (labeled checkpoint decisions by UTC day of the checkpoint), headline totals, checkpoint families, task-entry routes, recent events; `?include_fixtures=true` shows regression-test fixtures
- `GET /events[?type=jev.checkpoint&limit=200]` — newest-first event log with a type filter
- `GET /public/styles.css` — the Arc scaffold stylesheet, embedded at build time

Endpoints:

- `GET /health`
- `POST /api/audit/events`
- `GET /api/audit/events?limit=1000`
- `GET /api/audit/summary`
- `GET /api/audit/summary/checkpoints[?include_fixtures=true]`
- `GET /api/audit/summary/routes[?include_fixtures=true]`

## Grouped summaries

`/api/audit/summary` only counts event types, outcomes and token totals. The
grouped endpoints answer "how is each gate doing" without pulling
`/api/audit/events` and analysing it by hand.

`/api/audit/summary/checkpoints` groups `jev.checkpoint` events by `family`,
`question_version`, `policy_version`, `model_requested` and `model_returned`.
Each group reports:

- `count`, and `unlabeled` (`count - labeled`; an `unknown` outcome is unlabeled).
- `comparable` and `agreement_rate`: checkpoints with both a `baseline_action`
  and a `recommendation`, and the share where they are equal (`null` when none).
- `labeled`, `correct`, `accuracy` (`correct / labeled`, `null` when nothing is
  labeled) and `outcomes`, the distribution of joined outcomes. Outcomes come
  from `jev.checkpoint_outcome` events joined on `call_id`; when a call has
  several labels the latest one wins, and labels without a checkpoint are ignored.
- `recommendations`: distribution of the recommended choice.
- `top_pairs`: the five most common `baseline->recommendation` pairs.
- `median_confidence`, `median_latency_ms` (`null` when no sample exists).
- `tokens`: `total` of `jev_total_tokens` (or input plus output) and the number
  of `samples` behind it. Unmeasured events add nothing.

`/api/audit/summary/routes` groups `jev.route` events by `source`,
`routing_mode`, `model_requested` and `model_returned`, with the same count,
outcome (joined from `jev.outcome`), accuracy, latency and token fields, and
`recommendations` holding the `observed_recommendation` distribution.

Regression-test fixtures (`model_requested` or `model_returned` equal to
`fixture`) are excluded by default and reported in `excluded_fixture_events`;
pass `include_fixtures=true` to keep them.

```sh
curl http://127.0.0.1:18080/api/audit/summary/checkpoints
curl 'http://127.0.0.1:18080/api/audit/summary/routes?include_fixtures=true'
```

## Cross-machine

Every machine that runs the service has its own SQLite database, so summaries
only cover events emitted on that machine. To pool them, point a second host's
`HARNESS_AUDIT_URL` at one service over the tailnet, or copy the database file.
There is no sync between databases.

## Browser workbench

The service registers an Arc UI host (`src/ui.rs`) with the scaffold's admin
layout, `components/ui.html` macros and `public/styles.css`, all embedded with
`include_str!` so no working directory or asset build is needed. `GET /` renders
the grouped checkpoint and route summaries from `summary.rs` plus the newest
events; `GET /events` lists the log. Both pages read `audit_events_view`
through the same `ReadModelStore` as the JSON endpoints. There is no sign-in:
the service binds to loopback, and the framework's authenticated navigation is
replaced by the host's own links. The accuracy chart at the top is inline
SVG computed server-side by `summary::daily_checkpoint_accuracy` and
`ui::accuracy_chart`, so it needs no JavaScript; its daily counts sum to the
headline labeled and correct totals, and it pools families and question
versions, so use the checkpoint table to compare cohorts. Open http://127.0.0.1:18080/ after
`scripts/audit-service.sh serve` (a restart rebuilds the binary).
