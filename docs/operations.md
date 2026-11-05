# Operations

HooWire is early software. Deploy it first to an isolated qualification
environment and use the [compatibility report](compatibility.md) to select the
client workflows to evaluate.

## Configuration

The executable defaults to `serve` when no subcommand is given.

| Environment variable | Meaning | Default |
| --- | --- | --- |
| `DATABASE_URL` | PostgreSQL connection URL; required except for `keygen` | None |
| `HOOWIRE_MASTER_KEY` | Base64 of exactly 32 bytes; required for `serve`, `create-user` and `create-integration` | None |
| `HOOWIRE_BIND` | HTTP listening address | `127.0.0.1:8080` |
| `HOOWIRE_DATA_DIR` | Immutable asset directory | `data/assets` |
| `HOOWIRE_DB_MAX_CONNECTIONS` | Pool size, allowed range 2–100 | `10` |
| `HOOWIRE_MIGRATION_STATEMENT_TIMEOUT_SECS` | Per-statement budget for `migrate`, allowed range 1–3,600 seconds | `300` |
| `HOOWIRE_CORS_WEB_ORIGINS` | Comma-separated exact HTTP(S) web UI origins, at most 64 | Empty |
| `HOOWIRE_CORS_ALLOW_DESKTOP` | Allow literal `Origin: null` from opaque browser contexts | `true` |
| `RUST_LOG` | Rust tracing filter | `hoowire=info` |
| `HOOWIRE_USER_PASSWORD` | Password input for `create-user`, 12–1,024 bytes | None |
| `HOOWIRE_SDK_SECRET` | Optional explicit 32 ASCII byte secret for `create-integration` | Generated securely |

Global flags are `--database-url`, `--bind`, `--data-dir`,
`--db-max-connections`, `--cors-web-origins` and `--cors-allow-desktop`.
`migrate --statement-timeout-secs SECONDS` overrides its separate statement
budget. Prefer secret environment injection over flags that
would expose a URL or secret in a process listing. `--help` and each subcommand's
`--help` show the supported flags. `keygen` prints a fresh master key for the
operator to store securely. Protect it with the same care as a database backup.

One connection is reserved for the server-wide PostgreSQL listener. Allow for
all replicas and operator commands when setting PostgreSQL `max_connections`.
Serving and provisioning use a 15-second statement timeout. `migrate` uses
300 seconds by default and accepts 1–3,600 seconds. Both retain a 10-second lock
timeout, a 30-second idle transaction timeout and a 10-second pool acquire
timeout. The pool idle timeout is five minutes. PostgreSQL `application_name`
is `hoowire` for normal connections and `hoowire-migrate` for the migration
pool. Connection setup applies these settings explicitly; URL options do not
override them.

## Local development with Compose

The root README describes the initial build and explicit migration commands.
`compose.yaml` persists PostgreSQL and assets in named volumes. PostgreSQL has no
published host port; HooWire binds to host loopback. Required secret interpolation
fails when `.env` values are absent. The service runs without root privileges,
with a read-only root filesystem and a writable asset volume.

This configuration deliberately uses the database owner for convenience and
`sslmode=disable` within its private development network. Production uses
separate database roles and verified TLS as described below.

Changing `HOOWIRE_DB_PASSWORD` in `.env` does not rotate a password in an existing
PostgreSQL volume. Rotate the role password through PostgreSQL, then update the
configured secret. Do not remove persistent volumes as a password-reset method.

New Compose databases initialize host authentication with SCRAM-SHA-256,
including loopback TCP connections used by the authenticated `SELECT 1`
healthcheck. `POSTGRES_INITDB_ARGS` applies only when the data directory is
empty. Existing volumes retain their `pg_hba.conf`; review their host rules
explicitly when adopting this configuration. The healthcheck receives its
password through `PGPASSWORD` and emits no query or diagnostic output.

### Automated Compose smoke test

Build the image, then test the documented development configuration:

```sh
docker build -t hoowire:local .
./scripts/test-compose.sh hoowire:local
```

The script needs Bash, Git, curl, Docker and Docker Compose with `up --wait`
support. It uses the repository's `compose.yaml`, a unique project name, new
named volumes, a dynamic port on `127.0.0.1` and privately generated credentials.
It ignores the checkout's `.env` and the caller's HooWire credentials. The
optional image argument must select an image already present in the local
Docker daemon; the script does not build or publish an image.

Checks cover required secret interpolation, refusal to serve an unmigrated
database, explicit migration, CLI provisioning of a tenant, human account,
bot, directory group and role, plus integration credentials in a new private
file. The test verifies that an existing credential file is preserved when a
second provisioning command fails. Serving checks include live and ready
endpoints, the nonroot user, a read-only root filesystem, dropped capabilities,
and the writable asset volume. It stops and removes the containers while
preserving volumes, recreates them without another migration, and verifies
the account/directory records and a synthetic asset-volume file. Both server
stops must exit successfully after SIGTERM.

On exit, the script removes its own containers, network, named volumes and
temporary credentials. It leaves the selected image in the local daemon. This
test uses no Teamwire SDK or application and establishes the Compose lifecycle;
encrypted client workflows have their own [test layers](testing.md).

## Provision accounts

Provisioning is a trusted local operator action. There is no public registration
endpoint or bundled account administration UI. The default tenant UUID is
`00000000-0000-0000-0000-000000000001`. Create another tenant with:

```sh
hoowire create-tenant --name "Example organization"
```

This prints a UUID to use with the optional `--tenant UUID` on `create-user` and
`create-directory`. Tenant and directory provisioning need `DATABASE_URL` and
an up-to-date schema; they do not read the master key or create an asset store.

With `DATABASE_URL` and `HOOWIRE_MASTER_KEY` supplied securely, native commands
are:

```sh
hoowire create-user --email bot@example.test --first-name Bot --bot
hoowire create-user --email person@example.test --first-name Person
hoowire create-integration --user '<bot UUID>' --credentials-out /private/path/sdk.json
```

Set `HOOWIRE_USER_PASSWORD` before each `create-user` invocation and remove it
from the environment afterward. Account commands print a UUID. An integration
must belong to a bot. `create-integration` generates an App ID unless `--app-id`
is supplied and writes its App ID and secret to a new private file. It refuses
to overwrite an existing output file. Do not commit that file or put it in the
asset directory.

For the development Compose environment, forward an already exported password:

```sh
docker compose run --rm -e HOOWIRE_USER_PASSWORD hoowire \
  create-user --email bot@example.test --first-name Bot --bot
```

To save integration credentials to a host directory owned by your current user,
create a private directory outside the repository. This Linux/macOS example
uses a temporary provisioning data directory and working directory, so changing
the command's user does not affect the server asset volume:

```sh
mkdir -m 700 "$HOME/hoowire-operator-secrets"
docker compose run --rm \
  --user "$(id -u):$(id -g)" --workdir /tmp \
  --volume "$HOME/hoowire-operator-secrets:/secrets" \
  hoowire --data-dir /tmp/hoowire-provision \
  create-integration --user '<bot UUID>' --credentials-out /secrets/sdk.json
```

Directory groups and roles are local operator-managed entries:

```sh
hoowire create-directory --kind group --title Operations \
  --members '<user UUID>,<bot UUID>'
hoowire create-directory --kind role --title Coordinator \
  --tenant '<tenant UUID>' --members '<same-tenant user UUID>'
```

`--kind` accepts `group` or `role`. `--members` accepts comma-separated user
UUIDs; every member must belong to the selected tenant. Omitting `--tenant` uses
the default tenant. The command generates and prints a UUID unless `--id UUID`
is supplied. Supplying an existing ID for the same tenant and kind atomically
replaces its title and complete membership set. Omitting `--members` creates or
replaces the entry with an empty member set.

The optional SDK conformance tests require a bot integration and a second user.
Use a disposable instance; the suite creates and changes fixture data. See
[the SDK test procedure](sdk-compatibility.md#running-the-sdk-suite).

## Production PostgreSQL roles and TLS

Use one owner role for explicit migrations and a separate role for service
queries. The following role and database setup is run by a PostgreSQL
administrator; `\password` prompts without placing passwords in SQL files:

```sql
CREATE ROLE hoowire_owner LOGIN;
\password hoowire_owner
CREATE ROLE hoowire_runtime LOGIN;
\password hoowire_runtime
CREATE DATABASE hoowire OWNER hoowire_owner;
REVOKE ALL ON DATABASE hoowire FROM PUBLIC;
GRANT CONNECT ON DATABASE hoowire TO hoowire_owner, hoowire_runtime;
\connect hoowire
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO hoowire_runtime;
```

Apply migrations with `DATABASE_URL` set to the owner connection URL:

```sh
hoowire migrate
```

Plan enough time for each migration statement; the default five-minute budget
does not extend its ten-second lock-wait budget. Stop application writes before
migrating, inspect competing locks and rehearse large upgrades on a restored
copy. For a measured migration that needs a longer statement budget, use, for
example, `hoowire migrate --statement-timeout-secs 900`. SQLx's migration
advisory lock, checksum validation and transactional migration behavior remain
enabled. There is no unlimited migration timeout.

Then, as the owner, grant data access and future table privileges:

```sql
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO hoowire_runtime;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO hoowire_runtime;
REVOKE ALL ON TABLE public._sqlx_migrations FROM hoowire_runtime;
GRANT SELECT ON TABLE public._sqlx_migrations TO hoowire_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE hoowire_owner IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO hoowire_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE hoowire_owner IN SCHEMA public
    GRANT USAGE, SELECT ON SEQUENCES TO hoowire_runtime;
```

The runtime role cannot create schema objects or run migrations. It can read
migration versions and checksums so startup rejects a missing, incomplete or
outdated schema before binding the HTTP listener. The service needs access to
event counters, events, cursor mappings and resource tables; `LISTEN`/`NOTIFY`
supplements these durable records. Reapply explicit grants and review new tables
after upgrades. Default table privileges do not make the runtime a database owner.

Use a direct PostgreSQL connection or a proxy that preserves a dedicated
session for `LISTEN`. Transaction or statement pooling does not provide that
session guarantee. One reserved listener connection must remain available for
the lifetime of each serving process.

Keep PostgreSQL durability enabled (`fsync=on`, `full_page_writes=on` and
`synchronous_commit=on`) and use persistent storage with the required flush
semantics. HooWire does not override these database settings or provide storage
replication. Include them in the deployment's database qualification.

For connections across hosts, require certificate and hostname verification:

```text
postgres://hoowire_runtime:<URL-encoded-password>@db.example.net/hoowire?sslmode=verify-full&sslrootcert=/run/secrets/postgres-ca.pem
```

Mount the CA file read-only and use the DNS name covered by the database
certificate. Do not use the development `sslmode=disable` URL for that network.
Store connection URLs in your deployment's secret mechanism. Database and
application operators can access server-managed content and keys; this is not a
claim of protection from a compromised server operator.

## HTTPS and WSS

The application itself serves HTTP. Terminate TLS at a trusted proxy and keep
its HTTP listener private. `deploy/Caddyfile` provides a Caddy configuration for
an instance named `hoowire` on the same private container network. Set
`HOOWIRE_DOMAIN`, route DNS to Caddy and persist Caddy's certificate data.
Caddy handles WebSocket upgrades. If the proxy runs on the host, change the
upstream to `127.0.0.1:8080`.

Do not expose PostgreSQL or the unencrypted application port publicly. Set
appropriate proxy connection, request and rate limits for the expected clients.
The process admits at most 64 concurrent HTTP operations, four uploads and
1,024 WebSocket connections. Body limits are 16 KiB for session establishment,
6 MiB for encrypted JSON and 24 MiB for multipart uploads. Asset content itself
is limited to 16 MiB. Avatar content is limited to 2 MiB and 4,096 pixels on each
side. The supplied proxy request limit matches the largest outer request bound.

Health checks:

```sh
curl --fail http://127.0.0.1:8080/health/live
curl --fail http://127.0.0.1:8080/health/ready
curl --fail https://wire.example.net/health/ready
```

`live` indicates the process can answer HTTP. `ready` checks PostgreSQL. It does
not validate every protocol operation, an asset backup or client acceptance.
The runtime image intentionally has no HTTP probe utility; use the host,
reverse proxy or orchestrator for health requests.

## Browser and desktop origins

CORS is handled by the HTTP adapter. The default permits the literal opaque
`Origin: null`, with no hosted web UI origins. The isolated Desktop 5.0.1 probe
sent HTTP requests without an Origin header and WebSocket handshakes with
`Origin: file://`; it did not exercise HTTP CORS preflights. Independent HTTP
tests establish the documented CORS behavior.
For a web-only deployment, set `HOOWIRE_CORS_ALLOW_DESKTOP=false`. Add each
hosted web UI's exact serialized origin to `HOOWIRE_CORS_WEB_ORIGINS`, for example:

```text
HOOWIRE_CORS_WEB_ORIGINS=https://chat.example.org,http://localhost:3000
HOOWIRE_CORS_ALLOW_DESKTOP=false
```

Origins use `scheme://host[:port]`, without a path or trailing slash. Wildcards,
userinfo, queries, fragments and more than 64 entries are rejected at startup.
Use the browser's serialized `Origin` value; default ports can be normalized
away. Allowing `null` permits opaque origins generally, so it does not establish
that a request came from a particular desktop application.

Unauthenticated OPTIONS preflights allow GET, HEAD, POST, PUT, PATCH and DELETE,
and the `From`, `Content-Type`, `X-Request-ID` and `ETag` request headers. Responses
expose `ETag`; preflight caching is limited to 600 seconds. Responses vary by
origin, requested method and requested headers. The server does not send
`Access-Control-Allow-Credentials`. A disallowed origin receives no matching
Allow-Origin header, which the browser enforces. Authentication and tenant
authorization still apply independently to API operations.

## Upgrades and shutdown

Build with the committed `Cargo.lock`. Snapshot the three recovery components
below before changing a deployed revision. Stop serving traffic, run explicit
migrations with the owner role, review runtime grants, then start the new
executable with the runtime role. Validate readiness and selected SDK workflows
before returning traffic. A successful schema migration alone is not a client
compatibility result. There is no automatic downgrade of schema or data.

The executable handles SIGTERM and Ctrl-C through graceful HTTP shutdown.
HTTP operations have a 30-second bound. Active WebSocket sessions cancel their
current reads, database queries or event batches, then send close code `1001`
(`Server shutdown`). Flushing the close frame and waiting for the peer close
share a one-second bound. After the HTTP server drains, the executable waits
up to three seconds for every admitted WebSocket task to finish; a drain
timeout returns an error. Compose sends SIGTERM and allows 40 seconds before
forced termination. Verify shutdown with the actual long-lived clients used
in your environment.
Authenticated human sessions survive restart until their original one-hour
expiry if the same PostgreSQL data and master key are available. Unused
single-use WebSocket tickets are shared through PostgreSQL and remain usable
for their original 60-second lifetime, including at a different instance. Open
login handshakes are process-local and are lost on restart; completing one
still requires routing affinity. Clients obtain a fresh ticket after its expiry;
an expired login session requires authentication again. Durable event cursors
and messages remain in PostgreSQL.

## Backup and restore

A usable recovery set contains all three:

1. A consistent PostgreSQL backup, including migration state, events and cursors.
2. The immutable asset directory matching the database snapshot.
3. The original `HOOWIRE_MASTER_KEY` in a separately protected secret backup.

Quiesce writes for a straightforward coordinated database and filesystem
snapshot. Use PostgreSQL's supported logical or physical backup procedures and
retain their recovery metadata. An online filesystem copy taken independently
of an arbitrary database snapshot is not a demonstrated consistent backup.
Retain the build revision and runtime configuration with the recovery set.

`pg_dump` provides a consistent snapshot of one database, but does not include
cluster roles. Preserve the necessary roles separately with a protected
`pg_dumpall --globals-only` backup, or recreate them through the deployment's
role provisioning. Restore owner identities before restoring database objects,
then reapply and verify the runtime grants, including SELECT-only access to
`_sqlx_migrations`. Global dumps can contain password hashes and require
appropriate access controls.

Test restoration in an isolated environment. Restore PostgreSQL and the asset
directory, preserve owner and runtime privileges, supply the same master key,
and start the matching executable revision. Verify readiness, login, an SDK
message round trip, attachment digest checks and event replay before considering
the recovery set usable. This project does not currently include an automated
backup controller or a completed production recovery drill.

The database backup includes authenticated human sessions. Restoring a snapshot
can also restore a session that was revoked after that snapshot, while its
original expiry is still in the future. Decide the recovery policy before
opening traffic. To require fresh human logins, revoke restored sessions with
the migration owner in the isolated restored database:

```sql
-- Always discard short-lived upgrade credentials from a restored snapshot.
DELETE FROM websocket_tickets;

-- Apply when the recovery policy requires fresh human authentication.
UPDATE authenticated_sessions
SET revoked_at = clock_timestamp()
WHERE revoked_at IS NULL;
```

Discard pending tickets before starting a restored service; a snapshot can
otherwise resurrect a previously consumed ticket within its original expiry.
Apply the human-session revocation according to the selected recovery policy.
Integration credentials are
separate from these human session records. Expired and revoked session rows
are retained until explicit operator cleanup; there is no automatic pruning
or configurable per-user/global session quota.

Do not rotate the master key by replacing the environment variable. Stored
conversation, integration, human session and pending ticket envelopes require
the existing key.
A re-encryption migration is needed for key rotation; no automatic rotation
command is provided.
Keep obsolete assets and historical events until an explicit retention policy
accounts for their database references and client cursors.

### Automated logical restore test

`scripts/test-recovery.py` creates two fresh, isolated PostgreSQL containers.
It ignores an existing `DATABASE_URL`, generates its own test master key and
accounts, and uses a separate temporary asset directory. It does not contact
an existing HooWire instance or a Teamwire backend. Docker, Python 3.11 or later,
and a matching local HooWire executable are required:

```sh
cargo build --locked --bin hoowire
python3 -m venv .venv
.venv/bin/python -m pip install -r tests/requirements-recovery.txt
.venv/bin/python scripts/test-recovery.py
```

`HOOWIRE_TEST_POSTGRES_IMAGE` overrides the PostgreSQL 17 image;
`HOOWIRE_RECOVERY_BINARY` can select another local executable built from the
same schema revision. The crypto test dependency is separate from the server
runtime. No Teamwire SDK or application is required for this driver.

The test provisions a tenant, human account, bot integration, conversation,
message, avatar, attachment and authenticated human session. It stops the source
server before taking a custom-format `pg_dump` and copying immutable assets.
After restoring into the second fresh database, it compares every public table
count, verifies every fixture asset's size and SHA-256, starts the matching
executable and checks integration authentication, unsealed conversation keys,
downloaded attachment and avatar bytes, recovered human-session authentication,
subsequent logout revocation and the pre-backup message in the durable feed.

The later run on **5 November 2026, 06:27:34–06:27:46 UTC**
(**07:27:34–07:27:46 Europe/Berlin**) passed against PostgreSQL **17.11**,
with ten migrations and matching counts for all **24 public tables**. It used
Python **3.11.15** and `cryptography==50.0.0` installed in a new private virtual
environment. The recovery-only pin was updated after GitHub reported four
advisories against its previous version; the new pin is outside all four
reported vulnerable ranges. The separate external SDK environment was preserved.

Local ignored reports `artifacts/recovery-secure-smoke-20261105.json` and
`artifacts/recovery-secure-smoke-20261105-verification.json` record the results,
package provenance and cleanup. This run used the same 78-file source freeze
and executable as the [recorded Rust/SDK checks](testing.md#recorded-local-run-november-5-2026).
The recovery driver and dependency file are outside that manifest and were
separately verified before and after the run:

```text
recovery_driver_sha256=2dd5a7cca6cfefe44b531d5b1f395c60ecafb38e57d800c76fee4eb34d605c14
recovery_requirements_sha256=d2465f31a5a9d62553b258e7ce8650b44a3feb7e9fe36cf113aee8612b09f5a6
```

The preceding `recovery-smoke-20261105.json` and historical
`recovery-drill.json` reports were preserved. The test removes its own
containers and temporary files, including synthetic keys, credentials and
the private executable copy. No WebSocket tickets were present in the fixture;
this run does not qualify the pending-ticket invalidation policy above.

The selected test policy preserves its synthetic human session through restore
to verify encrypted-key recovery, then logs it out. Select the production
revocation policy described above independently. This is a small logical-restore
test with operator database privileges. It does not establish production backup
scheduling, role restoration, point-in-time recovery, host-loss recovery or
recovery time for a large dataset. Restricted runtime roles are checked separately
by `scripts/test-image.sh`.
