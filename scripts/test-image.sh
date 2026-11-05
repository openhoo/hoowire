#!/usr/bin/env bash
set -euo pipefail

# Qualify an existing local image against an owned, isolated database.
test_image=${1:-hoowire:local}
network_id=''
postgres_id=''
server_id=''
cleanup() {
    if [[ -n "$server_id" ]]; then docker rm -f "$server_id" >/dev/null 2>&1 || true; fi
    if [[ -n "$postgres_id" ]]; then docker rm -f -v "$postgres_id" >/dev/null 2>&1 || true; fi
    if [[ -n "$network_id" ]]; then docker network rm "$network_id" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

date -u '+measurement_time=%Y-%m-%dT%H:%M:%SZ'
docker image inspect "$test_image" --format 'image={{.Id}} image_bytes={{.Size}} platform={{.Os}}/{{.Architecture}}'
docker run --rm --network none --read-only --entrypoint /bin/sh "$test_image" -ec '
    test "$(id -u)" = 10001
    test "$(id -g)" = 10001
    test -s /etc/ssl/certs/ca-certificates.crt
    test -z "$(command -v python3 || true)"
    test -z "$(command -v node || true)"
    /usr/local/bin/hoowire --version
    /usr/local/bin/hoowire migrate --help
    printf "binary_bytes="
    wc -c < /usr/local/bin/hoowire
    ldd /usr/local/bin/hoowire
'
test_master_key=$(docker run --rm --network none --read-only "$test_image" keygen)
[[ ${#test_master_key} -eq 44 ]]

for invalid_timeout in 0 3601; do
    if parser_output=$(docker run --rm --network none --read-only \
        "$test_image" migrate --statement-timeout-secs "$invalid_timeout" 2>&1); then
        printf 'Migration CLI accepted an invalid statement timeout.\n' >&2
        exit 1
    fi
    [[ "$parser_output" == *'invalid value'* ]]
done

network_id=$(docker network create --internal \
    --label dev.openhoo.project=hoowire-image-verification \
    "hoowire-image-verification-$$")
postgres_id=$(docker run --detach --rm \
    --label dev.openhoo.project=hoowire-image-verification \
    --network "$network_id" --network-alias postgres \
    --env POSTGRES_USER=hoowire_admin \
    --env POSTGRES_PASSWORD=admin-disposable-local-only \
    --env POSTGRES_DB=hoowire \
    --env POSTGRES_INITDB_ARGS=--auth-host=scram-sha-256 \
    postgres:17.11-alpine)
postgres_ready=false
for ((attempt=0; attempt<60; attempt++)); do
    # The entrypoint temporarily starts a socket-only server before creating
    # POSTGRES_DB. Check the final TCP server and the actual database together.
    if docker exec --env PGPASSWORD=admin-disposable-local-only "$postgres_id" \
        psql -X -h 127.0.0.1 -U hoowire_admin -d hoowire \
        -v ON_ERROR_STOP=1 -Atqc 'SELECT 1' >/dev/null 2>&1; then
        postgres_ready=true
        break
    fi
    sleep 1
done
[[ "$postgres_ready" = true ]]

docker exec -i "$postgres_id" psql -v ON_ERROR_STOP=1 -U hoowire_admin -d hoowire <<'SQL'
CREATE ROLE hoowire_owner LOGIN PASSWORD 'owner-disposable-local-only';
CREATE ROLE hoowire_runtime LOGIN PASSWORD 'runtime-disposable-local-only';
ALTER DATABASE hoowire OWNER TO hoowire_owner;
REVOKE ALL ON DATABASE hoowire FROM PUBLIC;
GRANT CONNECT ON DATABASE hoowire TO hoowire_owner, hoowire_runtime;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO hoowire_runtime;
SQL

docker run --rm --network "$network_id" --read-only \
    --env DATABASE_URL=postgres://hoowire_owner:owner-disposable-local-only@postgres:5432/hoowire?sslmode=disable \
    "$test_image" migrate

docker exec -i "$postgres_id" psql -v ON_ERROR_STOP=1 -U hoowire_owner -d hoowire <<'SQL'
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO hoowire_runtime;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO hoowire_runtime;
REVOKE ALL ON TABLE public._sqlx_migrations FROM hoowire_runtime;
GRANT SELECT ON TABLE public._sqlx_migrations TO hoowire_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE hoowire_owner IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO hoowire_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE hoowire_owner IN SCHEMA public
    GRANT USAGE, SELECT ON SEQUENCES TO hoowire_runtime;
SQL
permission_result=$(docker exec "$postgres_id" psql -v ON_ERROR_STOP=1 -At -U hoowire_runtime -d hoowire -c "SELECT has_table_privilege('hoowire_runtime', '_sqlx_migrations', 'SELECT') AND NOT has_table_privilege('hoowire_runtime', '_sqlx_migrations', 'INSERT') AND NOT has_schema_privilege('hoowire_runtime', 'public', 'CREATE')")
[[ "$permission_result" = t ]]

test_tenant=$(docker run --rm --network "$network_id" --read-only \
    --env DATABASE_URL=postgres://hoowire_runtime:runtime-disposable-local-only@postgres:5432/hoowire?sslmode=disable \
    "$test_image" create-tenant --name 'Image verification')
[[ "$test_tenant" =~ ^[0-9a-f-]{36}$ ]]
test_user=$(docker run --rm --network "$network_id" --read-only \
    --tmpfs /data/assets:rw,uid=10001,gid=10001,mode=0700 \
    --env DATABASE_URL=postgres://hoowire_runtime:runtime-disposable-local-only@postgres:5432/hoowire?sslmode=disable \
    --env "HOOWIRE_MASTER_KEY=$test_master_key" \
    --env HOOWIRE_USER_PASSWORD=image-verification-disposable-only \
    "$test_image" create-user --tenant "$test_tenant" \
    --email image-check@example.test --first-name Image)
[[ "$test_user" =~ ^[0-9a-f-]{36}$ ]]
# Exercise the ticket repository's actual lock and consume statements with the
# documented runtime role. These bytes are synthetic SQL fixtures, not usable
# credentials. Keep both the consumed and unconsumed fixtures in a rollback.
ticket_permission_result=$(docker exec -i \
    --env PGPASSWORD=runtime-disposable-local-only "$postgres_id" \
    psql -X -w -h 127.0.0.1 -U hoowire_runtime -d hoowire \
    -v ON_ERROR_STOP=1 -v test_user="$test_user" -v test_tenant="$test_tenant" \
    -qAt <<'SQL'
SELECT current_user='hoowire_runtime' AND NOT rolsuper FROM pg_roles WHERE rolname=current_user;
BEGIN;
LOCK TABLE websocket_tickets IN SHARE ROW EXCLUSIVE MODE;
INSERT INTO websocket_tickets(token_hash,user_id,tenant_id,credential_sealed,expires_at)
VALUES(decode(repeat('ab',32),'hex'),:'test_user'::UUID,:'test_tenant'::UUID,
       decode('00112233','hex'),clock_timestamp()+interval '60 seconds');
WITH consumed AS (
    DELETE FROM websocket_tickets WHERE token_hash=decode(repeat('ab',32),'hex')
    RETURNING user_id,tenant_id,credential_sealed,expires_at
)
SELECT count(*)=1 AND bool_and(user_id=:'test_user'::UUID
    AND tenant_id=:'test_tenant'::UUID AND credential_sealed=decode('00112233','hex'))
FROM consumed WHERE expires_at>clock_timestamp();
WITH consumed AS (
    DELETE FROM websocket_tickets WHERE token_hash=decode(repeat('ab',32),'hex')
    RETURNING user_id,tenant_id,credential_sealed,expires_at
)
SELECT count(*)=0 FROM consumed WHERE expires_at>clock_timestamp();
INSERT INTO websocket_tickets(token_hash,user_id,tenant_id,credential_sealed,expires_at)
VALUES(decode(repeat('ac',32),'hex'),:'test_user'::UUID,:'test_tenant'::UUID,
       decode('00112233','hex'),clock_timestamp()+interval '60 seconds');
ROLLBACK;
SELECT count(*)=0 FROM websocket_tickets
WHERE token_hash IN (decode(repeat('ab',32),'hex'),decode(repeat('ac',32),'hex'));
SQL
)
[[ "$ticket_permission_result" = $'t\nt\nt\nt' ]]
printf 'WebSocket ticket runtime grants passed: table lock, insert, atomic consume, single use and rollback.\n'
test_group=$(docker run --rm --network "$network_id" --read-only \
    --env DATABASE_URL=postgres://hoowire_runtime:runtime-disposable-local-only@postgres:5432/hoowire?sslmode=disable \
    "$test_image" create-directory --tenant "$test_tenant" \
    --kind group --title 'Initial group' --members "$test_user")
[[ "$test_group" =~ ^[0-9a-f-]{36}$ ]]
docker run --rm --network "$network_id" --read-only \
    --env DATABASE_URL=postgres://hoowire_runtime:runtime-disposable-local-only@postgres:5432/hoowire?sslmode=disable \
    "$test_image" create-directory --tenant "$test_tenant" --id "$test_group" \
    --kind group --title 'Replaced group' >/dev/null
docker run --rm --network "$network_id" --read-only \
    --env DATABASE_URL=postgres://hoowire_runtime:runtime-disposable-local-only@postgres:5432/hoowire?sslmode=disable \
    "$test_image" create-directory --tenant "$test_tenant" \
    --kind role --title Coordinator --members "$test_user" >/dev/null
directory_result=$(docker exec "$postgres_id" psql -v ON_ERROR_STOP=1 -At -U hoowire_runtime -d hoowire -c "SELECT count(*) FILTER (WHERE kind='group')=0 AND count(*) FILTER (WHERE kind='role')=1 FROM directory_group_members WHERE tenant_id='$test_tenant'")
[[ "$directory_result" = t ]]

server_id=$(docker run --detach --read-only \
    --label dev.openhoo.project=hoowire-image-verification \
    --network "$network_id" --cap-drop ALL --security-opt no-new-privileges \
    --tmpfs /data/assets:rw,uid=10001,gid=10001,mode=0700 \
    --env DATABASE_URL=postgres://hoowire_runtime:runtime-disposable-local-only@postgres:5432/hoowire?sslmode=disable \
    --env "HOOWIRE_MASTER_KEY=$test_master_key" \
    "$test_image" serve)
health_request() {
    docker exec "$server_id" /bin/bash -ec '
        exec 3<>/dev/tcp/127.0.0.1/8080
        printf "GET %s HTTP/1.0\r\nHost: localhost\r\n\r\n" "$1" >&3
        IFS= read -r response <&3
        [[ "$response" = "HTTP/1.0 200"* || "$response" = "HTTP/1.1 200"* ]]
        printf "%s\n" "$response"
        cat <&3
    ' health-check "$1"
}
server_ready=false
for ((attempt=0; attempt<30; attempt++)); do
    if health_request /health/ready >/dev/null 2>&1; then
        server_ready=true
        break
    fi
    sleep 1
done
if [[ "$server_ready" != true ]]; then
    docker logs "$server_id" >&2 || true
    printf 'Server did not become ready with the documented runtime grants.\n' >&2
    exit 1
fi
health_request /health/live
health_request /health/ready
sleep 5
docker stats --no-stream --format 'idle_cpu={{.CPUPerc}} idle_memory={{.MemUsage}} idle_pids={{.PIDs}}' "$server_id"
docker stop --time 40 "$server_id" >/dev/null
shutdown_exit=$(docker inspect --format '{{.State.ExitCode}}' "$server_id")
[[ "$shutdown_exit" = 0 ]]
docker rm "$server_id" >/dev/null
server_id=''
printf '\nImage checks passed: nonroot, CA bundle, bounded migration CLI, provisioning, migration owner, restricted runtime role, WebSocket ticket lock/insert/consume, read-only serving, health endpoints and graceful shutdown.\n'
