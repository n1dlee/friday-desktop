CREATE TABLE settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    value TEXT NOT NULL CHECK (json_valid(value))
) STRICT;

CREATE TABLE history (
    turn_id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    input TEXT NOT NULL CHECK (length(CAST(input AS BLOB)) <= 8192),
    answer TEXT CHECK (answer IS NULL OR length(CAST(answer AS BLOB)) <= 16384),
    receipts TEXT NOT NULL CHECK (json_valid(receipts))
) STRICT;
CREATE INDEX history_created_at ON history (created_at, turn_id);

CREATE TABLE action_receipts (
    action_id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('open_app', 'output_volume')),
    state TEXT NOT NULL CHECK (state IN ('dispatching', 'accepted_by_os', 'volume_applied', 'blocked', 'failed', 'indeterminate'))
) STRICT;
CREATE INDEX action_receipts_created_at ON action_receipts (created_at, action_id);
