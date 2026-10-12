CREATE TABLE audit_events_view (
    id TEXT PRIMARY KEY NOT NULL,
    version BIGINT NOT NULL,
    data TEXT NOT NULL
);
