#!/usr/bin/env bash
set -euo pipefail

# Exercise the documented Compose lifecycle with a local image and owned volumes.
# Credentials stay in a private temporary directory and are never printed.
test_image=${1:-hoowire:local}
if [[ $# -gt 1 || ! "$test_image" =~ ^[A-Za-z0-9_./:@-]+$ ]]; then
    printf 'Usage: %s [local-image]\n' "$0" >&2
    exit 2
fi
repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d "${TMPDIR:-/tmp}/hoowire-compose.XXXXXXXX")
chmod 700 "$test_dir"
test_project="hoowire-compose-$(date +%s)-$$-$RANDOM"
probe_id=''
compose_started=false
cleanup() {
    if [[ -n "$probe_id" ]]; then docker rm -f "$probe_id" >/dev/null 2>&1 || true; fi
    if [[ "$compose_started" = true ]]; then
        compose down --volumes --remove-orphans >/dev/null 2>&1 || true
    fi
    rm -rf "$test_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Ignore caller configuration and the checkout's .env. Only the generated file
# selects this test's credentials, image and dynamic loopback port.
unset HOOWIRE_DB_PASSWORD HOOWIRE_MASTER_KEY HOOWIRE_PORT
unset HOOWIRE_DB_MAX_CONNECTIONS HOOWIRE_MIGRATION_STATEMENT_TIMEOUT_SECS
unset HOOWIRE_CORS_ALLOW_DESKTOP HOOWIRE_CORS_WEB_ORIGINS RUST_LOG
unset HOOWIRE_COMPOSE_TEST_IMAGE HOOWIRE_USER_PASSWORD HOOWIRE_SDK_SECRET
compose() {
    docker compose --project-name "$test_project" \
        --env-file "$test_dir/test.env" \
        --file "$repo_dir/compose.yaml" --file "$test_dir/override.yaml" "$@"
}
sql() {
    compose exec -T postgres psql -X -w -h 127.0.0.1 -U hoowire -d hoowire \
        -v ON_ERROR_STOP=1 -Atqc "$1"
}
uuid() { [[ "$1" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]; }
health() {
    local address
    address=$(compose port hoowire 8080)
    [[ "$address" =~ ^127\.0\.0\.1:[0-9]+$ ]]
    curl --noproxy '*' --silent --show-error --fail --max-time 2 \
        "http://$address$1" >/dev/null
}
wait_ready() {
    local attempt
    for ((attempt=0; attempt<60; attempt++)); do
        if health /health/ready >/dev/null 2>&1; then return; fi
        sleep 1
    done
    printf 'Compose service did not become ready within 60 seconds.\n' >&2
    return 1
}
check_provisioned() {
    [[ $(sql "SELECT count(*)=1 FROM tenants WHERE id='$test_tenant' AND name='Compose smoke'") = t ]]
    [[ $(sql "SELECT count(*)=2 AND count(*) FILTER (WHERE is_bot)=1 FROM users WHERE tenant_id='$test_tenant' AND id IN ('$test_user','$test_bot')") = t ]]
    [[ $(sql "SELECT count(*)=1 FROM integrations WHERE user_id='$test_bot' AND tenant_id='$test_tenant' AND length(secret_sealed)>0") = t ]]
    [[ $(sql "SELECT count(*)=1 FROM directory_groups WHERE tenant_id='$test_tenant' AND id='$test_group' AND kind='group' AND title='Replaced group'") = t ]]
    [[ $(sql "SELECT count(*)=1 AND bool_and(user_id='$test_user') FROM directory_group_members WHERE tenant_id='$test_tenant' AND group_id='$test_group' AND kind='group'") = t ]]
    [[ $(sql "SELECT count(*)=1 AND bool_and(user_id='$test_bot') FROM directory_group_members WHERE tenant_id='$test_tenant' AND group_id='$test_role' AND kind='role'") = t ]]
}

date -u '+measurement_time=%Y-%m-%dT%H:%M:%SZ'
printf 'compose_source_revision=%s\n' "$(git -C "$repo_dir" rev-parse HEAD)"
docker image inspect "$test_image" --format 'image={{.Id}} platform={{.Os}}/{{.Architecture}}'
printf 'compose_version=%s\n' "$(docker compose version --short)"
test_master_key=$(docker run --rm --network none --read-only "$test_image" keygen)
[[ ${#test_master_key} -eq 44 ]]
test_db_password=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
[[ ${#test_db_password} -eq 48 ]]
umask 077
cat > "$test_dir/test.env" <<EOF
HOOWIRE_DB_PASSWORD=$test_db_password
HOOWIRE_MASTER_KEY=$test_master_key
HOOWIRE_PORT=0
HOOWIRE_COMPOSE_TEST_IMAGE=$test_image
EOF
cat > "$test_dir/override.yaml" <<'YAML'
services:
  hoowire:
    image: ${HOOWIRE_COMPOSE_TEST_IMAGE:?}
    pull_policy: never
YAML
if HOOWIRE_DB_PASSWORD= HOOWIRE_MASTER_KEY= compose config --quiet \
    >"$test_dir/missing-secret.log" 2>&1; then
    printf 'Compose accepted missing required secrets.\n' >&2
    exit 1
fi
printf 'check=required-secret-interpolation passed\n'

compose_started=true
compose up --detach --wait --wait-timeout 90 postgres
postgres_id=$(compose ps --quiet postgres)
[[ -n "$postgres_id" ]]
[[ -z $(docker port "$postgres_id") ]]
[[ $(sql "SELECT count(*) FROM pg_tables WHERE schemaname='public'") = 0 ]]
probe_id=$(compose run --detach --no-deps hoowire serve)
probe_exited=false
for ((attempt=0; attempt<30; attempt++)); do
    if [[ $(docker inspect --format '{{.State.Status}}' "$probe_id") = exited ]]; then
        probe_exited=true
        break
    fi
    sleep 1
done
[[ "$probe_exited" = true ]]
[[ $(docker inspect --format '{{.State.ExitCode}}' "$probe_id") = 1 ]]
[[ $(sql "SELECT count(*) FROM pg_tables WHERE schemaname='public'") = 0 ]]
docker rm "$probe_id" >/dev/null
probe_id=''
printf 'check=serve-rejects-unmigrated-database passed\n'

compose run --rm -T --no-deps hoowire migrate
[[ $(sql 'SELECT count(*)>0 AND bool_and(success) FROM _sqlx_migrations') = t ]]
printf 'postgres_version=%s\n' "$(sql 'SHOW server_version')"
printf 'check=explicit-migrate passed\n'

test_tenant=$(compose run --rm -T --no-deps hoowire create-tenant --name 'Compose smoke')
uuid "$test_tenant"
export HOOWIRE_USER_PASSWORD
HOOWIRE_USER_PASSWORD=$(docker run --rm --network none --read-only "$test_image" keygen)
test_user=$(compose run --rm -T --no-deps -e HOOWIRE_USER_PASSWORD hoowire \
    create-user --tenant "$test_tenant" --email person@example.invalid --first-name Person)
uuid "$test_user"
HOOWIRE_USER_PASSWORD=$(docker run --rm --network none --read-only "$test_image" keygen)
test_bot=$(compose run --rm -T --no-deps -e HOOWIRE_USER_PASSWORD hoowire \
    create-user --tenant "$test_tenant" --email bot@example.invalid --first-name Bot --bot)
unset HOOWIRE_USER_PASSWORD
uuid "$test_bot"
test_group=$(compose run --rm -T --no-deps hoowire create-directory --tenant "$test_tenant" \
    --kind group --title 'Initial group' --members "$test_user,$test_bot")
uuid "$test_group"
[[ $(sql "SELECT count(*) FROM directory_group_members WHERE tenant_id='$test_tenant' AND group_id='$test_group'") = 2 ]]
compose run --rm -T --no-deps hoowire create-directory --tenant "$test_tenant" \
    --id "$test_group" --kind group --title 'Replaced group' --members "$test_user" >/dev/null
test_role=$(compose run --rm -T --no-deps hoowire create-directory --tenant "$test_tenant" \
    --kind role --title Coordinator --members "$test_bot")
uuid "$test_role"

mkdir -m 700 "$test_dir/secrets"
integration() {
    compose run --rm -T --no-deps --user "$(id -u):$(id -g)" --workdir /tmp \
        --volume "$test_dir/secrets:/secrets" hoowire --data-dir /tmp/hoowire-provision \
        create-integration --user "$test_bot" --credentials-out /secrets/sdk.json
}
integration >/dev/null
[[ -s "$test_dir/secrets/sdk.json" ]]
compose run --rm -T --no-deps --entrypoint /bin/sh \
    --user "$(id -u):$(id -g)" --workdir /tmp \
    --volume "$test_dir/secrets:/secrets:ro" hoowire \
    -ec 'test "$(stat -c %a /secrets/sdk.json)" = 600'
cp "$test_dir/secrets/sdk.json" "$test_dir/secrets/before.json"
if integration >"$test_dir/overwrite.log" 2>&1; then
    printf 'Integration command overwrote an existing credentials file.\n' >&2
    exit 1
fi
cmp -s "$test_dir/secrets/before.json" "$test_dir/secrets/sdk.json"
check_provisioned
printf 'check=tenant-human-bot-directory-and-private-integration-provisioning passed\n'

compose up --detach --no-build hoowire
wait_ready
health /health/live
server_id=$(compose ps --quiet hoowire)
[[ $(docker inspect --format '{{.Config.User}} {{.HostConfig.ReadonlyRootfs}}' "$server_id") = '10001:10001 true' ]]
[[ $(docker inspect --format '{{json .HostConfig.CapDrop}}' "$server_id") = '["ALL"]' ]]
[[ $(docker inspect --format '{{json .HostConfig.SecurityOpt}}' "$server_id") = '["no-new-privileges:true"]' ]]
compose exec -T hoowire /bin/sh -ec '
    test "$(id -u)" = 10001
    test "$(id -g)" = 10001
    test "$(stat -c %u:%g:%a /data/assets)" = 10001:10001:700
    if touch /etc/hoowire-compose-write-check 2>/dev/null; then exit 1; fi
    printf "compose-persistence-check\n" > /data/assets/.compose-smoke
    test "$(cat /data/assets/.compose-smoke)" = compose-persistence-check
'
printf 'check=loopback-health-nonroot-readonly-and-writable-assets passed\n'

compose stop --timeout 40 hoowire
[[ $(docker inspect --format '{{.State.ExitCode}}' "$server_id") = 0 ]]
compose down --remove-orphans
compose up --detach --wait --wait-timeout 90 postgres
compose up --detach --no-build hoowire
wait_ready
health /health/live
restarted_id=$(compose ps --quiet hoowire)
[[ -n "$restarted_id" && "$restarted_id" != "$server_id" ]]
compose exec -T hoowire /bin/sh -ec \
    'test "$(cat /data/assets/.compose-smoke)" = compose-persistence-check'
check_provisioned
printf 'check=database-and-asset-persistence-after-container-recreation passed\n'
compose stop --timeout 40 hoowire
[[ $(docker inspect --format '{{.State.ExitCode}}' "$restarted_id") = 0 ]]
printf 'check=graceful-sigterm-exit-zero passed\n'

# Assert cleanup, including both named volumes and the private network.
compose down --volumes --remove-orphans
[[ -z $(docker ps --all --quiet --filter "label=com.docker.compose.project=$test_project") ]]
[[ -z $(docker volume ls --quiet --filter "label=com.docker.compose.project=$test_project") ]]
[[ -z $(docker network ls --quiet --filter "label=com.docker.compose.project=$test_project") ]]
compose_started=false
printf 'check=owned-container-volume-and-network-cleanup passed\n'
printf 'Compose checks passed. No SDK or vendor application was used.\n'
