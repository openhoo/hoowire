-- The wire format requires timestamp ETags. Persist their exact durable outbox
-- position instead of using wall-clock time to filter concurrently written rows.
CREATE TABLE sync_cursors (
    user_id UUID NOT NULL REFERENCES users(id),
    token TIMESTAMPTZ NOT NULL,
    position BIGINT NOT NULL CHECK(position >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(user_id,token)
);
CREATE INDEX sync_cursors_expiry ON sync_cursors(user_id,created_at);
