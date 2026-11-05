ALTER TABLE assets ADD CONSTRAINT assets_id_tenant UNIQUE(id,tenant_id);
CREATE TABLE asset_shares (
    asset_id UUID NOT NULL,
    chat_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    PRIMARY KEY(asset_id,chat_id),
    FOREIGN KEY(asset_id,tenant_id) REFERENCES assets(id,tenant_id),
    FOREIGN KEY(chat_id,tenant_id) REFERENCES chats(id,tenant_id)
);
CREATE INDEX asset_shares_chat ON asset_shares(chat_id,asset_id);
INSERT INTO asset_shares(asset_id,chat_id,tenant_id)
    SELECT id,chat_id,tenant_id FROM assets WHERE chat_id IS NOT NULL;
