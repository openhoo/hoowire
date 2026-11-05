-- Preserve the original request identity across reactions, metadata edits and
-- tombstones without retaining another cleartext copy of deleted message data.
ALTER TABLE messages ADD COLUMN request_hash BYTEA CHECK(request_hash IS NULL OR octet_length(request_hash)=32);
