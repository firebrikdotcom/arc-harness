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

Endpoints:

- `GET /health`
- `POST /api/audit/events`
- `GET /api/audit/events?limit=1000`
- `GET /api/audit/summary`
