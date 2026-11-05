CREATE TABLE tenants (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO tenants(id,name) VALUES ('00000000-0000-0000-0000-000000000001','Default');

CREATE TABLE users (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    email_hash TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    profile JSONB NOT NULL DEFAULT '{}',
    is_bot BOOLEAN NOT NULL DEFAULT false,
    preferences JSONB NOT NULL DEFAULT '{}',
    event_sequence BIGINT NOT NULL DEFAULT 0 CHECK (event_sequence >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(id,tenant_id),
    CHECK (jsonb_typeof(profile) = 'object'),
    CHECK (jsonb_typeof(preferences) = 'object')
);
CREATE INDEX users_directory ON users(tenant_id,id);

CREATE TABLE integrations (
    id UUID PRIMARY KEY,
    app_id TEXT NOT NULL UNIQUE,
    secret_sealed TEXT NOT NULL,
    user_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    FOREIGN KEY(user_id,tenant_id) REFERENCES users(id,tenant_id),
    CHECK (length(app_id) BETWEEN 1 AND 512),
    CHECK (length(secret_sealed) > 0)
);

CREATE TABLE chats (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    created_by UUID NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL,
    col TEXT NOT NULL,
    version BIGINT NOT NULL DEFAULT 1 CHECK(version > 0),
    attributes JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(id,tenant_id),
    FOREIGN KEY(created_by,tenant_id) REFERENCES users(id,tenant_id),
    CHECK(jsonb_typeof(attributes) = 'object')
);
CREATE TABLE chat_members (
    chat_id UUID NOT NULL,
    user_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    is_admin BOOLEAN NOT NULL DEFAULT false,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_message_id UUID,
    read_at TIMESTAMPTZ,
    PRIMARY KEY(chat_id,user_id),
    FOREIGN KEY(chat_id,tenant_id) REFERENCES chats(id,tenant_id),
    FOREIGN KEY(user_id,tenant_id) REFERENCES users(id,tenant_id)
);
CREATE INDEX chat_members_by_user ON chat_members(user_id,chat_id);

CREATE TABLE messages (
    id UUID PRIMARY KEY,
    chat_id UUID NOT NULL REFERENCES chats(id),
    sender_id UUID NOT NULL REFERENCES users(id),
    client_id TEXT,
    body TEXT NOT NULL,
    attributes JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ,
    UNIQUE(sender_id,chat_id,client_id),
    UNIQUE(id,chat_id),
    CHECK(jsonb_typeof(attributes) = 'object')
);
CREATE INDEX messages_history ON messages(chat_id,created_at,id);
CREATE INDEX messages_by_sender ON messages(sender_id,chat_id);
ALTER TABLE chat_members ADD CONSTRAINT read_message_in_chat
    FOREIGN KEY(read_message_id,chat_id) REFERENCES messages(id,chat_id);

-- IDs are monotonic per user. A user-row counter commits atomically with each
-- event, so a cursor never skips an earlier sequence from an uncommitted writer.
CREATE TABLE events (
    user_id UUID NOT NULL REFERENCES users(id),
    id BIGINT NOT NULL CHECK(id > 0),
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(user_id,id)
);

CREATE TABLE assets (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    uploaded_by UUID NOT NULL,
    chat_id UUID,
    content_type TEXT NOT NULL,
    size BIGINT NOT NULL CHECK(size >= 0),
    digest TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY(uploaded_by,tenant_id) REFERENCES users(id,tenant_id),
    FOREIGN KEY(chat_id,tenant_id) REFERENCES chats(id,tenant_id)
);
CREATE INDEX assets_by_chat ON assets(chat_id,id);

CREATE TABLE directory_groups (
    id UUID NOT NULL,
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    title TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('group','role')),
    attributes JSONB NOT NULL DEFAULT '{}',
    PRIMARY KEY(tenant_id,id,kind),
    CHECK(jsonb_typeof(attributes) = 'object')
);
CREATE TABLE directory_group_members (
    tenant_id UUID NOT NULL,
    group_id UUID NOT NULL,
    kind TEXT NOT NULL,
    user_id UUID NOT NULL,
    PRIMARY KEY(tenant_id,group_id,kind,user_id),
    FOREIGN KEY(tenant_id,group_id,kind) REFERENCES directory_groups(tenant_id,id,kind) ON DELETE CASCADE,
    FOREIGN KEY(user_id,tenant_id) REFERENCES users(id,tenant_id)
);
CREATE INDEX directory_members_by_user ON directory_group_members(user_id,group_id);
