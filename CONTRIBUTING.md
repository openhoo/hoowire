# Contributing

Use the Rust version in `rust-toolchain.toml`. Keep `Cargo.lock` committed.
The server and default test suite require no Node.js or Python runtime.
See [testing](docs/testing.md) for the test layers, focused commands and the
separate external SDK, container, recovery and native-client evidence.

Before proposing a change:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
./scripts/test-postgres.sh
```

The PostgreSQL script starts and removes its own disposable container. With an
existing disposable database, set `HOOWIRE_TEST_DATABASE_URL` and run
`cargo test --workspace --locked -- --ignored` instead. Never point tests at an
operator's or production database.
The opt-in run includes real HTTP/WebSocket integration and, on Unix, actual
executable lifecycle and SIGTERM tests. A default Rust run skips these ignored
database-dependent cases; it still runs filesystem contracts and CLI checks
that fail before a database connection.

For packaging changes, build the image and run
`./scripts/test-image.sh hoowire:local`. It uses and removes its own PostgreSQL
container and internal network, including separate owner and runtime roles.

Keep domain records independent from SQL and wire formatting. Apply mutations,
authorization checks and durable events in one PostgreSQL transaction. Bind SQL
values. Preserve tenant constraints, optimistic versions, idempotency, immutable
blob publication and per-user event ordering. Add a numbered migration rather
than editing an already applied migration.

For protocol changes, add a regression from an independently observed contract,
then record the exact SDK version, fixture and result in the compatibility
reports. A route existing or a synthetic test passing does not establish full
SDK compatibility. Keep untested and unsupported operations visible. The
external SDK suite is an optional manual test and requires an independently
obtained SDK; see [SDK evidence](docs/sdk-compatibility.md).

Do not commit SDK wheels, vendor applications, extracted vendor code, chat
content, account data, database dumps, private keys or integration credentials.
Contributions to HooWire are licensed under Apache-2.0 unless separately agreed.
