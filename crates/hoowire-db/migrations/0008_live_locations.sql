-- Location coordinates remain encrypted with the conversation's client key.
CREATE TABLE live_locations (
    message_id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    chat_id UUID NOT NULL,
    user_id UUID NOT NULL,
    location TEXT NOT NULL CHECK(octet_length(location) BETWEEN 1 AND 65536),
    duration BIGINT NOT NULL CHECK(duration BETWEEN 1 AND 2678400),
    map_overlay TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    last_active_at TIMESTAMPTZ NOT NULL,
    stopped_at TIMESTAMPTZ,
    FOREIGN KEY(message_id,chat_id) REFERENCES messages(id,chat_id),
    FOREIGN KEY(chat_id,tenant_id) REFERENCES chats(id,tenant_id),
    FOREIGN KEY(user_id,tenant_id) REFERENCES users(id,tenant_id)
);
CREATE INDEX live_locations_by_chat ON live_locations(chat_id,expires_at,message_id) WHERE stopped_at IS NULL;
CREATE INDEX live_locations_by_user ON live_locations(user_id,expires_at,message_id) WHERE stopped_at IS NULL;
