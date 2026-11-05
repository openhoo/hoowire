-- A durable per-tenant watermark for full directory resynchronization.
-- This is a logical millisecond clock: rapid changes advance it by at least
-- one millisecond, preserving ordering in the native numeric resync marker.
CREATE TABLE directory_revisions (
    tenant_id UUID PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    changed_at TIMESTAMPTZ NOT NULL
);
INSERT INTO directory_revisions(tenant_id,changed_at)
SELECT id,date_trunc('milliseconds',clock_timestamp()) FROM tenants;

CREATE FUNCTION advance_directory_revision() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    affected_tenant UUID;
BEGIN
    IF TG_OP='DELETE' THEN
        affected_tenant := OLD.tenant_id;
    ELSE
        affected_tenant := NEW.tenant_id;
    END IF;
    INSERT INTO directory_revisions(tenant_id,changed_at)
    VALUES(affected_tenant,date_trunc('milliseconds',clock_timestamp()))
    ON CONFLICT(tenant_id) DO UPDATE
    SET changed_at=GREATEST(EXCLUDED.changed_at,directory_revisions.changed_at+INTERVAL '1 millisecond');
    RETURN NULL;
END;
$$;

CREATE TRIGGER directory_users_revision
AFTER INSERT OR DELETE OR UPDATE OF profile,is_bot,email_hash ON users
FOR EACH ROW EXECUTE FUNCTION advance_directory_revision();
CREATE TRIGGER directory_groups_revision
AFTER INSERT OR UPDATE OR DELETE ON directory_groups
FOR EACH ROW EXECUTE FUNCTION advance_directory_revision();
CREATE TRIGGER directory_group_members_revision
AFTER INSERT OR UPDATE OR DELETE ON directory_group_members
FOR EACH ROW EXECUTE FUNCTION advance_directory_revision();
