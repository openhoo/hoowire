#!/usr/bin/env bash
# Test against an owned, disposable PostgreSQL container on a dynamic local port.
set -euo pipefail

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
container_id=''
cleanup() {
    if [[ -n "$container_id" ]]; then
        docker rm --force --volumes "$container_id" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cd "$project_dir"
test_password=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
test_image=${HOOWIRE_TEST_POSTGRES_IMAGE:-postgres:17-alpine}
container_id=$(docker run --detach --rm \
    --label dev.openhoo.project=hoowire-disposable-tests \
    --publish 127.0.0.1::5432 \
    --env POSTGRES_USER=hoowire \
    --env POSTGRES_DB=hoowire \
    --env POSTGRES_INITDB_ARGS=--auth-host=scram-sha-256 \
    --env "POSTGRES_PASSWORD=$test_password" \
    "$test_image")

printf 'Waiting for the isolated PostgreSQL test database...\n'
database_ready=false
for ((attempt=0; attempt<60; attempt++)); do
    # Avoid observing the socket-only initialization server before DB creation.
    if docker exec --env "PGPASSWORD=$test_password" "$container_id" \
        psql -X -h 127.0.0.1 -U hoowire -d hoowire \
        -v ON_ERROR_STOP=1 -Atqc 'SELECT 1' >/dev/null 2>&1; then
        database_ready=true
        break
    fi
    sleep 1
done
if [[ "$database_ready" != true ]]; then
    printf 'PostgreSQL did not become ready within 60 seconds.\n' >&2
    exit 1
fi
address=$(docker port "$container_id" 5432/tcp)
test_port=${address##*:}
case "$test_port" in
    ''|*[!0-9]*) printf 'Cannot determine the PostgreSQL loopback port.\n' >&2; exit 1 ;;
esac
export HOOWIRE_TEST_DATABASE_URL="postgres://hoowire:$test_password@127.0.0.1:$test_port/hoowire"
printf 'Running Rust tests, including PostgreSQL integration tests.\n'
cargo test --workspace --locked "$@" -- --include-ignored
