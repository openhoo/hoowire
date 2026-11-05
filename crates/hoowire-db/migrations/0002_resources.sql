-- Independent wire resources. Their mutations use the same chat lock and
-- per-user transactional outbox as messages and membership changes.
CREATE TABLE wire_reactions (
    id UUID PRIMARY KEY,
    message_id UUID NOT NULL,
    chat_id UUID NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id),
    emoji TEXT NOT NULL CHECK(length(emoji) BETWEEN 1 AND 128),
    action TEXT NOT NULL CHECK(action IN ('React','Unreact')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(message_id,user_id),
    FOREIGN KEY(message_id,chat_id) REFERENCES messages(id,chat_id)
);
CREATE INDEX wire_reactions_sync ON wire_reactions(chat_id,updated_at,id);

CREATE TABLE poll_records (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    created_by UUID NOT NULL,
    question TEXT NOT NULL CHECK(length(question) BETWEEN 1 AND 8192),
    duration BIGINT NOT NULL CHECK(duration BETWEEN 0 AND 2678400),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    ended_at TIMESTAMPTZ,
    FOREIGN KEY(created_by,tenant_id) REFERENCES users(id,tenant_id)
);
CREATE TABLE poll_choices (
    poll_id UUID NOT NULL REFERENCES poll_records(id) ON DELETE CASCADE,
    choice_id INTEGER NOT NULL CHECK(choice_id BETWEEN 0 AND 19),
    label TEXT NOT NULL CHECK(length(label) BETWEEN 1 AND 2048),
    PRIMARY KEY(poll_id,choice_id)
);
CREATE TABLE poll_votes (
    poll_id UUID NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id),
    choice_id INTEGER NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(poll_id,user_id),
    FOREIGN KEY(poll_id,choice_id) REFERENCES poll_choices(poll_id,choice_id)
);
CREATE TABLE poll_chats (
    poll_id UUID NOT NULL REFERENCES poll_records(id) ON DELETE CASCADE,
    chat_id UUID NOT NULL REFERENCES chats(id),
    PRIMARY KEY(poll_id,chat_id)
);
CREATE INDEX poll_chats_by_chat ON poll_chats(chat_id,poll_id);

CREATE TABLE chat_pins (
    chat_id UUID NOT NULL,
    message_id UUID NOT NULL,
    pinned_by UUID NOT NULL REFERENCES users(id),
    pinned_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(chat_id,message_id),
    FOREIGN KEY(message_id,chat_id) REFERENCES messages(id,chat_id)
);
CREATE INDEX chat_pins_by_time ON chat_pins(chat_id,pinned_at,message_id);
