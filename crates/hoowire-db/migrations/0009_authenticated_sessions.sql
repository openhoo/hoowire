-- Human From credentials are stored as SHA-256 hashes. The service seals the
-- transport key with the server master key and binds it to the token and owner.
CREATE TABLE authenticated_sessions (
    token_hash BYTEA PRIMARY KEY CHECK(octet_length(token_hash)=32),
    user_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    key_sealed BYTEA NOT NULL CHECK(octet_length(key_sealed)=64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY(user_id,tenant_id) REFERENCES users(id,tenant_id) ON DELETE CASCADE,
    CHECK(expires_at>created_at)
);
CREATE INDEX authenticated_sessions_by_user ON authenticated_sessions(user_id,expires_at);
CREATE INDEX authenticated_sessions_expiry ON authenticated_sessions(expires_at);
