# PostgreSQL persistence

HooWire uses PostgreSQL for its durable records. `hoowire-db` contains the
schema, parameterized SQL and transaction boundaries. No Redis, broker or
application cache is required for correctness. A bounded SQLx connection pool
has an acquire timeout, idle expiry, statement timeout, lock timeout and a
30-second idle transaction timeout. Connections use the `hoowire` application
name in PostgreSQL activity views. Explicit migration connections instead use
`hoowire-migrate` and a configurable 1–3,600 second statement budget, default
300 seconds; the serving statement budget remains 15 seconds.

`Repository::migrate` embeds the migration SQL. SQLx's PostgreSQL migration
advisory lock and checksum validation remain enabled. Run migrations with a
role allowed to create tables; the service role can be restricted afterward.
Migrations are explicit operator commands. Serving validates the embedded
migration versions, checksums and success state before binding HTTP. Back up
PostgreSQL together with blob storage and the server master key before upgrades.

## Integrity and isolation

Users belong to tenants. Composite foreign keys on chat memberships, chat
creators, assets, integrations, authenticated sessions, WebSocket tickets and
directory memberships
reject relationships between tenants. Chats are locked before mutations. Only
conversation members read messages and shared attachment metadata. Conversation
metadata and administrative membership changes require an administrator.
Authors and administrators can delete or update message metadata. Reactions
are separately scoped to the acting member. One-to-one conversations are created
with exactly two participants; administrative membership changes cannot change
their participant set. A current member can voluntarily unsubscribe, including
from a one-to-one conversation. Administrative membership changes preserve an
administrator. An administrator leaving while other members remain must first
appoint another administrator; a sole participant can leave an empty
conversation record.

Messages carry a unique `(sender_id, chat_id, client_id)` idempotency key when
provided by the SDK. A repeated request returns the original message; reusing
the key with different content returns a conflict. A chat version implements
optimistic concurrency for clients that supply an expected `version`. The
subscription's `chat_v`/`chatV` is a separate protocol-mode field and remains
zero for the implemented legacy chat protocol; it never carries that database
counter. Positive protocol modes are explicitly unsupported. Removal,
message writes, tombstones, read markers and corresponding events commit in a
single transaction. A failed permission or constraint check leaves no event.

Message deletion clears body and attributes and retains a tombstone. Other
server backups can still contain earlier data. Asset bytes are managed by the
blob crate; PostgreSQL stores authorization, size, digest and media type. Private
SDK uploads gain conversation access only when an authorized sender posts an
attachment reference. `asset_shares` preserves separate grants for forwarding.
Guessing another user's private asset UUID cannot grant access, and failed
attachment validation rolls back the message and every new share together.

## Integration secrets

The service encrypts integration protocol keys with its AES-256-GCM master key
before calling the repository. `Integration.secret` at this boundary contains
the sealed envelope, stored in `integrations.secret_sealed`. Conversation `col` keys are likewise
sealed and opened by the service at the repository boundary. The repository does
not decrypt it. Passwords are password hashes. Database errors are mapped to
application errors; submitted values and SQL diagnostics are not returned or
logged.

Administrative shutdown, crash shutdown and recovery-in-progress SQLSTATEs map
to temporary unavailability (HTTP 503). A terminated in-flight query does not
require restarting the application; the pool can reconnect for later requests.
The interruption test locks its own profile row, observes the blocked backend,
terminates that backend and checks both rollback and a later successful write.
It does not establish whether an arbitrary connection loss happened before or
after a commit. Message retries should preserve their original idempotency key.

## Authenticated human sessions

`authenticated_sessions` stores a SHA-256 hash of the human `From` token as its
32-byte primary key. Raw tokens are not stored. The service encrypts the
32-byte transport key with AES-256-GCM using the master key; the 64-byte `HWS1`
envelope includes a random nonce and authentication tag. Associated data binds
the envelope to its token hash, tenant and user, so swapping envelopes between
session records fails authentication. A composite foreign key binds the session
owner to the tenant.

Login sets a fixed one-hour lifetime using PostgreSQL `clock_timestamp()`.
Every authentication reads PostgreSQL and checks expiry and revocation without
an authenticated session cache. The same database and master key allow an
unexpired session to survive process restart or be authenticated by another
instance. Logout records revocation and sends a transactional user notification
so connected WebSockets can recheck it; a periodic 15-second check covers missed
notifications. Single-use WebSocket tickets reauthenticate their underlying
credential when consumed.

Pending login handshakes live only in process memory for 120 seconds, with
capacity 4,096 and at most five login attempts each. They are lost on restart.
Authenticated database session records have no per-user or global quota.
Expired and revoked records remain until explicit operator cleanup; automatic
pruning, renewal and a device/session-management API are not implemented. A
restored backup can restore pre-revocation state; see the explicit recovery
policy in [operations](operations.md#backup-and-restore).

## Single-use WebSocket tickets

Migration `0010_websocket_tickets.sql` stores a SHA-256 ticket digest, a tenant-bound
owner, sealed credential bytes and an expiry. The URL-safe random token stays
43 characters long. The credential uses an AES-256-GCM `HWT1` envelope with its
own associated-data domain bound to the digest, tenant and user. Raw tickets,
raw `From` credentials and session keys are not stored in this table. A service
instance with the same database and master key can consume another instance's
ticket, including after the issuer restarts, within its original lifetime.

Issuance sets a fixed 60-second lifetime using PostgreSQL time. A transaction
serializes issuance with a table lock, removes expired rows and enforces a
global maximum of 4,096 pending tickets; sealed payloads are also size bounded.
Atomic `DELETE ... RETURNING` allows exactly one consumer, and an expired row
never yields credentials. Consumption opens the envelope and reauthenticates
the original credential against current session expiry and revocation. Logout
therefore invalidates an unused ticket on every instance. Expired rows are
removed on issuance or attempted consumption.

An operator restore can resurrect a previously consumed ticket while its
original expiry is still in the future. Discard pending tickets before opening
restored traffic, as described in [recovery](operations.md#backup-and-restore).

## Durable events and bounds

Each user has a monotonic event sequence, updated under a row lock in the same
transaction that inserts the event. There is no global PostgreSQL sequence whose
uncommitted IDs could be skipped by a reconnecting cursor. Shared conversation
events acquire recipient counters in sorted UUID order. Four bulk statements
lock the complete recipient set, increment its counters, insert its events and
issue commit-only notifications. Replay queries use the
`(user_id,id)` primary key and an exclusive `after` cursor. HTTP feed ETags retain
their durable outbox position across process restarts. The WebSocket loop uses
a connection-local position, starts reading at zero and starts again at zero
on reconnect. No client acknowledgement or resume-cursor input restores its
previous connection position.
Event transactions also issue a PostgreSQL `NOTIFY` containing only the recipient
UUID. Notifications are delivered on commit and are wake-up hints; replay reads
the durable outbox. One process-wide `PgListener` reserves one pool connection,
so serving configurations need at least two connections. A listener reconnect
and periodic replay recover dropped notifications.

Read queries have a maximum of 1,000 records. Message history orders by creation
time and UUID; the SDK timestamp filter is exclusive. A timestamp alone cannot
represent a position between messages that share a timestamp. The durable event
cursor is the precise replay mechanism. Legacy timestamp ETags are persisted in
`sync_cursors` with their exact per-user outbox position. The feed locks the same
user counter as event writers, snapshots a bounded event batch and stores its
new timestamp token atomically. Concurrent commits and equal message timestamps
cannot fall between timestamp filters. Idle polls reuse an unchanged valid token without inserting another row.
Tokens expire after 30 days; an unknown
or expired token starts a bounded replay from zero. Directory and conversation listing are
bounded; additional explicit pagination is needed before larger directories can
be considered fully supported. Events are retained until an explicit retention
policy is implemented; do not truncate them independently of client cursors.

## Real PostgreSQL tests

Use a fresh database dedicated to tests. Tests create isolated users and
conversations and do not truncate unrelated records.

```sh
HOOWIRE_TEST_DATABASE_URL='postgres://hoowire:password@127.0.0.1:55439/hoowire' \
  cargo test -p hoowire-db -p hoowire-service --locked -- --ignored
```

The explicitly ignored tests require PostgreSQL and fail if the connection
variable is absent. They exercise tenant isolation, authorization, rollbacks,
concurrent idempotent requests, event replay after reconnect, version conflicts,
last-admin protection, immutable direct chats, deletion, reactions, read markers,
attachment permissions, profiles, preferences, sealed secrets and directory
foreign keys, migration connection settings and durable authenticated sessions
across service instances. They do not substitute an in-memory database.
