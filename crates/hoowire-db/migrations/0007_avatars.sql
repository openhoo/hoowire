-- Avatars are small and commit atomically with their owner's metadata.
-- The service encrypts bytes with authenticated encryption before persistence.
CREATE TABLE avatars (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    user_id UUID UNIQUE,
    chat_id UUID UNIQUE,
    mime_type TEXT NOT NULL CHECK(mime_type IN ('image/png','image/jpeg','image/gif')),
    bytes_sealed BYTEA NOT NULL CHECK(octet_length(bytes_sealed) BETWEEN 33 AND 2097184),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK(num_nonnulls(user_id,chat_id)=1),
    FOREIGN KEY(user_id,tenant_id) REFERENCES users(id,tenant_id),
    FOREIGN KEY(chat_id,tenant_id) REFERENCES chats(id,tenant_id)
);
CREATE INDEX avatars_by_tenant ON avatars(tenant_id);
