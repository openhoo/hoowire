-- Single-use upgrade credentials are shared between serving instances. Only a
-- SHA-256 token digest and master-key-sealed credential material enter SQL.
CREATE TABLE websocket_tickets (
    token_hash BYTEA PRIMARY KEY CHECK(octet_length(token_hash)=32),
    user_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    credential_sealed BYTEA NOT NULL CHECK(octet_length(credential_sealed) BETWEEN 1 AND 4096),
    expires_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY(user_id,tenant_id) REFERENCES users(id,tenant_id) ON DELETE CASCADE
);
CREATE INDEX websocket_tickets_expiry ON websocket_tickets(expires_at);
