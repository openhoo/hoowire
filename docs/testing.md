# Testing HooWire

Reviewed: **November 5, 2026**. Historical client and artifact results retain
their own recorded dates and source identities.

HooWire has several test layers. Each exercises a different boundary and keeps
its own evidence. A passing Rust test, an unmodified SDK scenario and a workflow
in a released client establish different things. None establishes complete
Teamwire compatibility or production readiness.

The commands below run from the repository root. Test totals change as cases are
added; use the test runner's summaries for the exact source revision being
tested. A test count is not a coverage percentage or a compatibility certificate.

## Recorded local run: November 5, 2026

The later smoke qualification includes migration `0010` and the shared
WebSocket-ticket fixes in commit `86dbe5b4140174bfd24a19cccfed21f0ca37aab9`.
The frozen source/fixture manifest contains 78 files with SHA-256
`5bfdfb8a2ce102834812d2aa1403ede070f9c2bf43cf84c3210013b99e467e0b`.
The executable used for the SDK and restore checks has SHA-256
`552289f5dce21cb61f1207fb9521cd6aa0cb934d9702c261769f8e160232b63e`.
The date here uses Europe/Berlin; the machine-readable reports use UTC.

| Check | Recorded result |
| --- | --- |
| Formatting, all-target Clippy with warnings denied, debug build | Passed |
| Default Rust suite without PostgreSQL | 35 passed, 70 database-dependent cases skipped |
| Full Rust suite against disposable PostgreSQL 17.11 | 105 passed, zero failed or skipped; includes eleven executable process cases |
| Unmodified external SDK 2.0.0, strict complete mode | 111 scenarios passed, one pre-HTTP SDK failure, one dependent scenario blocked; 84 methods attempted, 83 qualified; exit 1 |
| Separate authenticated SDK WebSocket SIGTERM probe | Close 1001, peer close reply and server exit 0 |
| Synthetic logical database/asset restore | All 24 table counts preserved, ten migrations accepted, recovered authentication, keys, bytes and replay checked; repeated successfully with recovery-only `cryptography==50.0.0` |
| Final Linux ARM64 image | Passed, including authenticated restricted-runtime-role ticket SQL and idle shutdown |
| Development Compose lifecycle | All eight check groups passed, including persistence across container recreation |

The default and PostgreSQL totals overlap: 105 is the complete Rust total, not
an additional 105 tests. No line or branch coverage percentage was measured.
The reference SDK failure is explained in [SDK evidence](sdk-compatibility.md).
The image check has its own [artifact record](runtime-measurements.md); the
native desktop result remains associated with its earlier source snapshot in
[client evidence](client-contract.md).

The local machine-readable reports are under the ignored `artifacts/`
directory: `source-freeze-smoke-20261105.json`,
`rust-verification-smoke-20261105.json`, `sdk-conformance-smoke-20261105.json`,
`sdk-shutdown-smoke-20261105.json`, `recovery-smoke-20261105.json`,
`recovery-secure-smoke-20261105.json`,
`image-smoke-20261105.json` and `compose-smoke-20261105.json`. They are not installed
runtime dependencies or a substitute for rerunning the suite on a new revision.

The earlier 91-test run and SDK/restore results retain their 72-file source
digest `d8fe4652d00b97c0100a15898430f1cb8fdee891e331838363b37619b1ab3e7a`.
The earlier SDK/restore executable has digest
`3d876bf8a9635fd1c0eb1653eb72beab44f441a8c18e0c93f2c320b6cd00ba0a`;
the separately built Linux image executable has digest
`167e5c39740c366002f9240932d6a7be61093fa71852441c091eb68a82a8a10f`.
Their `*-final.json`, `sdk-shutdown-smoke.json` and `recovery-drill.json`
reports were preserved; the new qualification uses separate report names.

### Reproduced failures and regressions

- The first public CI run intermittently timed out waiting for a WebSocket
  event. The original test binary reproduced the failure on its seventh local
  attempt. The test now decrypts and checks the subscription acknowledgement
  before posting on a separate HTTP connection. Twenty subsequent repetitions
  passed without increasing the timeout.
- A new two-process E2E received HTTP `403` when redeeming a ticket on the
  process that had not issued it. PostgreSQL now stores sealed ticket credentials
  and atomically consumes their hashes. Repository, service and process tests
  cover expiry, capacity, tampering, logout, issuer restart and concurrent
  consumption. The two-process race requires exactly one `101` and one `403`.
- Terminating the PostgreSQL backend of a confirmed blocked profile write
  initially returned an internal error. Shutdown/recovery SQLSTATEs `57P01`,
  `57P02` and `57P03` now map to service unavailable. The regression verifies
  the classified error, unchanged original data, pool recovery and a later
  successful write. It does not introduce automatic mutation retries.

## Test layers

| Layer | Driver and real dependencies | Evidence and limits |
| --- | --- | --- |
| Rust tests without PostgreSQL | Rust unit tests, public crypto/domain/wire contracts, real temporary filesystem and selected CLI child processes | Validation, independent crypto vectors, serialization, immutable blob publication and early configuration rejection; no database-backed workflows |
| PostgreSQL and service integration | Public repository/service APIs against real PostgreSQL | Transactions, tenant isolation, ACLs, optimistic versions, idempotency, durable events, resource expiry and authenticated sessions; no released client UI |
| HTTP and WebSocket integration | Independent Rust drivers, real loopback HTTP/WS listeners, PostgreSQL and temporary assets | Encrypted wire requests, authentication, CORS, binary/text frames, access checks and selected protocol errors; not execution of the external SDK |
| Executable E2E | The actual Cargo-built `hoowire` child processes, PostgreSQL, public encrypted HTTP/WS and files | Configuration, explicit migration, human chat/file lifecycle, process restart, cross-process tickets/events/revocation and SIGTERM close handshake; Unix-only, synthetic clients on one host |
| External SDK | Independently installed, unmodified Teamwire Python SDK 2.0.0 and a disposable HooWire instance | The named scenarios and SDK methods in its JSON report; incomplete scenarios and the documented reference SDK failure remain visible |
| Container image | An already built local image and an owned PostgreSQL container/internal network | Actual runtime filesystem, roles, CLI provisioning, read-only serving, health, idle shutdown and size observations; no active SDK/WebSocket clients in this script |
| Development Compose | An already built image, the committed Compose file, owned project/network/volumes and disposable PostgreSQL | Explicit migration, human/bot/integration/directory provisioning, private credential files, read-only serving and persistence across container recreation; no SDK or released client |
| Logical recovery | Actual executable, two fresh PostgreSQL containers, asset copy and an independent Python crypto driver | Quiesced synthetic dump/restore and recovered authentication/bytes/replay; no production PITR, host-loss or large-dataset qualification |
| Historical native probe | Disclosed isolated Teamwire desktop 5.0.1 on macOS | Only the recorded workflows and source snapshot; not a rerunnable Cargo test, pristine-install qualification or mobile evidence |

## Rust tests without PostgreSQL

Install the pinned Rust **1.93.1** toolchain and its `rustfmt` and `clippy`
components from [rust-toolchain.toml](../rust-toolchain.toml). A working native
linker and access to the locked Cargo dependencies are needed for a clean build.
These tests require no running PostgreSQL server, Node.js or Python.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The default test command skips `#[ignore]` database-dependent tests. It still
runs filesystem contract tests and, on Unix, the CLI configuration cases that
fail before a database connection is needed. It is broader than a unit-only run.

Examples of focused checks:

```sh
cargo test -p hoowire-domain --locked --test validation
cargo test -p hoowire-crypto --locked --test public_contract
cargo test -p hoowire-blob --locked --test storage
cargo test -p hoowire-api --locked --test wire_models
cargo test -p hoowire --locked --test startup
```

The [crypto contracts](../crates/hoowire-crypto/tests/public_contract.rs) include
independent vectors, block boundaries, truncation, padding, canonical Base64 and
invalid ECDH inputs. The [blob contracts](../crates/hoowire-blob/tests/storage.rs)
use a temporary filesystem to test concurrent publication, empty/exact-limit
objects, digest corruption and I/O failures. Python is not needed to execute
the committed crypto vectors. Wire-model contracts protect server-owned fields
and deletion tombstones while preserving opaque extension attributes.

## Real PostgreSQL, HTTP and WebSocket tests

The simplest full Rust run additionally needs Bash and a working Docker daemon:

```sh
./scripts/test-postgres.sh
```

The [script](../scripts/test-postgres.sh) starts a fresh PostgreSQL 17 container
on a dynamically allocated loopback port, generates its disposable password,
waits for an authenticated TCP query against the target database, exports
`HOOWIRE_TEST_DATABASE_URL`, then runs all Rust tests with `--include-ignored`.
It removes its own container and volumes on exit. The database role has the
privileges needed to apply migrations and construct test fixtures.

`HOOWIRE_TEST_POSTGRES_IMAGE` selects another available PostgreSQL 17 image.
Cargo arguments can narrow the run while keeping the owned database lifecycle:

```sh
HOOWIRE_TEST_POSTGRES_IMAGE=postgres:17.11-alpine ./scripts/test-postgres.sh
./scripts/test-postgres.sh -p hoowire-db --test adversarial
./scripts/test-postgres.sh -p hoowire-api --test interoperability
```

With an existing **disposable** PostgreSQL database, explicitly select it:

```sh
export HOOWIRE_TEST_DATABASE_URL='<disposable PostgreSQL URL>'
cargo test --workspace --locked -- --ignored
```

`--ignored` runs only the opt-in tests; `--include-ignored` runs both default and
opt-in tests. `DATABASE_URL` alone does not select the integration-test database.
Tests create persistent fixtures, apply migrations and sometimes adjust fixture
timestamps or construct corrupt test records. Never select an operational or
production database. Cleanup of a caller-supplied database is the caller's task.

Focused commands with the selected database:

```sh
cargo test -p hoowire-db --locked --test postgres -- --ignored
cargo test -p hoowire-db --locked --test resources -- --ignored
cargo test -p hoowire-db --locked --test adversarial -- --ignored
cargo test -p hoowire-db --locked --test feed_cursors -- --ignored
cargo test -p hoowire-db --locked --test pool_settings -- --ignored
cargo test -p hoowire-db --locked --test availability -- --ignored
cargo test -p hoowire-db --locked --test websocket_tickets -- --ignored
cargo test -p hoowire-service --locked --test sessions -- --ignored
cargo test -p hoowire-service --locked --test websocket_tickets -- --ignored
cargo test -p hoowire-api --locked --test interoperability -- --ignored
cargo test -p hoowire-api --locked --test cors -- --ignored
cargo test -p hoowire-api --locked --test transport_failures -- --ignored
```

Repository tests include mixed-resource rollback, stale versions without event
leaks, concurrent ACL removal and mutations, conflicting concurrent idempotency
payloads, resource ownership, tenant boundaries, event sequence ordering and
expiry. Service tests check durable sessions across fresh instances, shared
logout revocation, expiry and sealed-key tampering. HTTP/WS tests use actual
network sockets, encrypted requests and durable state, including malformed
envelopes and credential failures. Transport cases also exercise invalid feed
cursors, continued valid requests after errors, and bounded request/upload
admission while keeping liveness or unrelated requests available. The admission
scenarios use a shared time budget below the server request deadline. Exact
JSON/encrypted-body boundaries, malformed multipart fields and the 16 MiB file
limit are checked through HTTP, followed by valid requests and byte verification.
Their drivers are independent Rust test code.

## Actual executable E2E and shutdown

These tests use `CARGO_BIN_EXE_hoowire`, which Cargo builds for the integration
test. They start an owned server process on loopback with a temporary asset
directory. Database-dependent cases need `HOOWIRE_TEST_DATABASE_URL` as above:

```sh
cargo test -p hoowire --locked --test startup -- --include-ignored
cargo test -p hoowire --locked --test lifecycle -- --ignored
cargo test -p hoowire --locked --test operations -- --ignored
cargo test -p hoowire --locked --test shutdown -- --ignored
```

[Startup tests](../crates/hoowire/tests/startup.rs) cover rejected CLI arguments,
pool/CORS configuration, missing database configuration, invalid master keys
and refusing an unmigrated schema. The successful path applies explicit CLI
migration, starts the server, checks readiness and stops it.

[Lifecycle tests](../crates/hoowire/tests/lifecycle.rs) provision synthetic
accounts, exercise human ECDH login, contacts and group creation, deliver nested
encrypted text and a file, verify private-upload denial before message sharing,
then check original sessions, conversation keys, history and bytes across
process restarts. Logout remains revoked after another restart. A separate
scenario checks other-user and cross-tenant requests through real HTTP.
Migrations and tenant creation use CLI commands; account fixtures use the
service provisioning API. This is not a released-client UI test.

The [operations tests](../crates/hoowire/tests/operations.rs) start two owned
server processes with the same database and asset directory. They check human
login, bidirectional encrypted events and identical history across processes,
remote logout invalidating HTTP, pending tickets and an idle WebSocket while a
second session remains valid, and ticket consumption after issuer shutdown
and restart. Concurrent upgrade requests must produce exactly one successful
upgrade. These selected same-host scenarios do not qualify a complete cluster
or routing setup; pending ECDH handshakes still need routing affinity.

The [shutdown test](../crates/hoowire/tests/shutdown.rs) upgrades authenticated
WebSockets, verifies encrypted subscription acknowledgements, sends SIGTERM to
the owned executable and requires close code `1001`. It briefly withholds peer
close replies to check that the process waits, then completes the handshakes
and requires successful exit. This complements the image script's idle stop.

These process tests are compiled only on Unix and require the `kill` command.
Their process, readiness, HTTP and WebSocket waits have explicit deadlines, and
owned child processes/assets are cleaned up on failure. Windows process
lifecycle qualification remains separate.

## Unmodified external SDK

This optional manual layer needs a disposable running HooWire server, fixture
accounts/integrations, Python matching the independently obtained SDK wheel,
and that wheel's declared dependencies. [SDK requirements](../tests/requirements-sdk.txt)
describe installation; the repository does not distribute the SDK or its wheel.
Use the fixture provisioning instructions and environment variables in
[Running the SDK suite](sdk-compatibility.md#running-the-sdk-suite).

After building the selected source, record its source/fixture identity and pass
the resulting `source_sha256` into the run:

```sh
python tests/source_manifest.py --output target/sdk-source.json
export HOOWIRE_TEST_SERVER_SOURCE_SHA256='<source_sha256 from that manifest>'
python tests/sdk_conformance.py \
  --extended --avatars --reactions --live-location --notifications \
  --require-complete --report target/sdk-conformance.json
```

The second disposable bot is needed for incoming notification cases, and
circle/role fixtures are needed for those methods. The suite defaults to
loopback; a deliberately selected remote test host requires `--allow-remote`.
Keep the external SDK unmodified and preserve failures. The reference SDK's
`Chat.update_am_reaction` failure occurs before an HTTP request and is recorded
in [SDK evidence](sdk-compatibility.md#reference-sdk-defect).

The JSON report distinguishes passed, failed, blocked, attempted and unqualified
methods. `--require-complete` fails on methods missing from successful scenarios;
it does not prove all parameter, callback, permission or reconnect combinations.
Recreate the manifest and report after included source or fixture changes. A
source digest identifies a snapshot; it is not remote binary attestation.

## Container image and logical recovery

Container checks require Docker and a built image, with no SDK installation:

```sh
docker build --tag hoowire:local .
./scripts/test-image.sh hoowire:local
./scripts/test-compose.sh hoowire:local
```

The [image script](../scripts/test-image.sh) uses an owned internal network and
PostgreSQL container, separate migration-owner/runtime roles, the actual image's
CLI, UID/GID `10001:10001`, a read-only root filesystem and a writable temporary
asset mount. It checks documented grants, provisioning, health and successful
idle SIGTERM exit within a 40-second grace. It also prints artifact sizes and one
Docker idle-memory/CPU/PIDs observation. That memory display is not process RSS,
and no throughput or connected-client load is measured. Recorded observations
and their exact artifact identities are in [runtime measurements](runtime-measurements.md).

The [Compose script](../scripts/test-compose.sh) uses the committed development
configuration with private generated secrets, a unique project, fresh volumes
and a dynamic loopback port. Its eight groups check secret/schema failures,
explicit migration, account/integration/directory provisioning, credential-file
permissions and overwrite rejection, serving restrictions, persistent records
and asset-volume bytes after recreation, SIGTERM and owned-resource cleanup.
See [Compose operations](operations.md#automated-compose-smoke-test) for prerequisites.

The logical restore drill additionally needs Python 3.11 or later and its
separate crypto dependency:

```sh
cargo build --locked --bin hoowire
python3 -m venv .venv
.venv/bin/python -m pip install -r tests/requirements-recovery.txt
.venv/bin/python scripts/test-recovery.py
```

[Recovery instructions](operations.md#automated-logical-restore-test) describe
the owned containers, binary/image overrides and checks. The driver ignores an
existing `DATABASE_URL`, creates its own keys and accounts, quiesces the source,
copies a logical dump and assets into a fresh database, and verifies recovered
keys, authentication, exact file bytes and durable replay. It writes
`artifacts/recovery-drill.json`, then removes its own containers and temporary
credentials/files. The synthetic session is preserved to verify recovery, then
explicitly logged out. Production revocation policy is operator-selected.
The later recorded run uses the separately installed recovery dependency
`cryptography==50.0.0`; it preserves the SDK's vendor-declared dependencies.
The recovery driver and requirement file have separate recorded hashes because
the source/fixture manifest does not include them. See the recovery instructions
for these identities and the retained historical reports. Its fixture contains
no pending WebSocket tickets, so it does not test their restore invalidation.

The drill uses operator database privileges. It does not qualify production
backup scheduling, role restoration, PITR, host-loss recovery or recovery time
for a large dataset. The separate image test checks restricted runtime roles.

## Historical native-client evidence

The [desktop contract](client-contract.md) and
[compatibility report](compatibility.md#reference-clients-and-evidence) describe
the November 4, 2026 desktop 5.0.1 probe and its recorded source snapshot. It
used a separate bundle/profile and disclosed OS/egress isolation adapters. Its
peer was an independent human HTTP driver. It did not use a second released app.

Preserve that attribution when evaluating current changes. Passing automated
tests or rebuilding HooWire does not renew the historical native result. The
remaining native, mobile, multi-device and resynchronization gaps are listed
in [compatibility status](compatibility.md#work-still-required-for-normal-teamwire-applications).

## CI and reporting

The [GitHub workflow](../.github/workflows/ci.yml) defines formatting, Clippy,
default Rust tests, opt-in PostgreSQL/HTTP/WS/process tests and a release build on
Ubuntu. Its PostgreSQL service has a finite readiness check and the job has an
overall timeout. These are workflow definitions; a particular CI result must
still be tied to its actual revision and run.

The SDK, image, logical recovery and native layers are separate manual checks;
the workflow does not execute them. Keep their reports with the relevant source
and executable/image identities, toolchain/client versions, fixture selection,
commands, exit results and known limitations. Do not promote a filtered run to
a full-suite result, or an old report to evidence for a changed executable.
