# HooWire

An independent Rust server targeting interoperability with **Teamwire Python
SDK 2.0.0 and Backend API 20**. One native executable, PostgreSQL and immutable
local asset storage. The serving process needs no Node.js, Python, Redis or
message broker.

**Early software. Full Teamwire compatibility and production readiness have not
been established.** The [compatibility report](docs/compatibility.md),
[SDK evidence](docs/sdk-compatibility.md) and executable tests distinguish tested
behavior from unverified or unsupported features. Throughput and latency remain
unmeasured. [Runtime measurements](docs/runtime-measurements.md) record executable
and image sizes plus one explicitly bounded idle observation.

## Structure

| Crate | Responsibility |
| --- | --- |
| `hoowire` | Executable, configuration and operator commands |
| `hoowire-api` | API 20 HTTP and WebSocket adapters |
| `hoowire-service` | Authentication, sessions, authorization and use cases |
| `hoowire-domain` | Domain records, validation and errors |
| `hoowire-db` | PostgreSQL transactions, migrations and durable events |
| `hoowire-crypto` | SDK transport and session key exchange |
| `hoowire-blob` | Immutable local assets with digest verification |

Conversation and membership changes, message writes, read markers, attachments,
polls, reactions and pins use PostgreSQL transactions. Events have durable
per-user cursors; one PostgreSQL listener wakes WebSockets after commit.
Timestamp ETags map to persisted event positions. SDK retries use idempotency
keys. Integration and conversation secrets are sealed with AES-256-GCM.
Authenticated human sessions persist in PostgreSQL with hashed tokens and
sealed transport keys; logout revocation is shared between instances.

## Start locally

Requirements: Docker with Compose. Native development additionally uses the
pinned Rust **1.93.1** toolchain.

```sh
docker build -t hoowire:local .
docker run --rm hoowire:local keygen
cp .env.example .env
```

Put the generated value in `HOOWIRE_MASTER_KEY` in `.env`, and choose a unique,
random URL-safe `HOOWIRE_DB_PASSWORD`. Keep `.env` private.

```sh
docker compose up -d postgres
docker compose run --rm hoowire migrate
docker compose up -d hoowire
curl --fail http://127.0.0.1:8080/health/ready
```

Migrations are an explicit operator action. `serve` does not apply them.
The development Compose file shares one database owner account between the
commands and service, disables database TLS inside its private network and
publishes HTTP only on loopback. Use the separate database roles and HTTPS/WSS
configuration in [operations](docs/operations.md) for deployment.

Provision accounts and SDK integration credentials using the CLI described in
[operations](docs/operations.md#provision-accounts). There is no bundled web UI
or public self-registration flow.

Additional organizations and directory entries can be provisioned locally:

```sh
docker compose run --rm hoowire create-tenant --name "Example organization"
docker compose run --rm hoowire create-directory --kind group --title Operations \
  --members '<user UUID>,<bot UUID>'
```

`create-directory` also accepts `--kind role`, an optional `--tenant UUID` and
`--id UUID` to replace an existing entry's title and complete member set. Members
must belong to the selected tenant; omitting `--tenant` uses the default tenant.
See [provisioning](docs/operations.md#provision-accounts) for the account commands
and credential inputs.

## Build and test

```sh
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
./scripts/test-postgres.sh
```

The PostgreSQL script starts a disposable real database on a dynamic loopback
port, runs all Rust tests including database integration tests and removes only
its own container. Use `HOOWIRE_TEST_POSTGRES_IMAGE` to select an available
PostgreSQL 17 image. CI runs formatting, Clippy, default Rust tests, real PostgreSQL
integration tests, HTTP/WebSocket checks, Unix process E2E and a release build.
See [testing](docs/testing.md) for focused commands, prerequisites and the
evidence provided by each layer.

The recorded local run on **November 5, 2026** passed **91 Rust tests**, including
eight executable process E2E cases, against disposable PostgreSQL 17.11.
Formatting and Clippy with warnings denied also passed. The test report records
the exact source snapshot and the separate SDK and native-client evidence.

After building the container, `./scripts/test-image.sh hoowire:local` checks
the actual image with its own isolated PostgreSQL container and private network.
It checks migration-owner/runtime grants, local provisioning, nonroot read-only
serving, health endpoints and graceful shutdown, then removes its resources.
It also prints image size, executable size and one bounded idle container sample.

The optional [logical restore test](docs/operations.md#automated-logical-restore-test)
creates two fresh PostgreSQL containers, provisions its own synthetic accounts
and resources, backs up and restores the database and assets, then checks
authentication, recovered keys, file bytes and durable replay. It uses an
independent Python crypto driver and needs no Teamwire SDK installation.

The optional [SDK conformance suite](docs/sdk-compatibility.md#running-the-sdk-suite)
executes an independently installed, unmodified SDK. The SDK and its wheel are
not distributed here. Missing scenarios remain visible in the report;
`--require-complete` makes incomplete method coverage fail explicitly.

## Documentation

- [Architecture](docs/architecture.md)
- [Test layers, commands and evidence](docs/testing.md)
- [Deployment, configuration and recovery](docs/operations.md)
- [Measured artifact sizes and idle observation](docs/runtime-measurements.md)
- [Database integrity and durable events](docs/data.md)
- [Compatibility status](docs/compatibility.md)
- [SDK evidence and test procedure](docs/sdk-compatibility.md)
- [Observed desktop contract](docs/client-contract.md)
- [Contributing](CONTRIBUTING.md) and [security reporting](SECURITY.md)

## License and independence

HooWire's original code is licensed under **Apache-2.0**; see [LICENSE](LICENSE)
and [NOTICE](NOTICE). Dependencies retain their own licenses. Teamwire's SDK
and applications have separate terms. HooWire is not affiliated with or endorsed
by Teamwire GmbH.

The earlier OpenHoo desktop modification project is now **Teamwire Patcher**
(`openhoo/teamwire-patcher`). Its patching runtime is separate from this server.
