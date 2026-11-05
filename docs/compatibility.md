# Compatibility status

Last reviewed: **5 November 2026**. HooWire implements an independently observed
subset of Teamwire Backend API 20. The target includes the official Python SDK
2.0.0 and ordinary human accounts in desktop and mobile applications.
**Full compatibility has not been established.** An implemented route, an SDK
parser accepting a fixture, and a successful real-client workflow are different
levels of evidence.

## Reference clients and evidence

| Evidence | What it establishes | Limits |
| --- | --- | --- |
| Official Python SDK 2.0.0, unmodified external wheel | HTTP paths, payload shapes, encryption, SDK parsing and scenarios in `tests/sdk_conformance.py` | Read the generated report for passed, failed, blocked and unexercised methods; no whole-SDK certificate |
| Desktop application 5.0.1, macOS arm64, disclosed isolation build | Actual UI password login, human contact selection, group creation, text send, live incoming text, renderer reload/history, one native file upload/display and logout; existing session survives own server restart | Separate bundle/profile, in-memory key store and egress/OS-effect adapters; no renderer/protocol/parser changes; peer is an independent human HTTP driver, not a second released app. See `artifacts/native-desktop-probe.json`; other desktop workflows and mobile remain unqualified |
| Independent crypto vectors and public contract tests | secp256k1 key derivation, AES-CBC boundary/padding vectors generated with Python `cryptography`, canonical Base64 and malformed envelopes/keys | Does not qualify a full login or client workflow |
| Rust HTTP/WebSocket integration tests against PostgreSQL | Real encrypted requests, durable writes, access checks, SDK-style binary WebSocket delivery and human login/logout for tested scenarios | The test driver is independent Rust code, not a released Teamwire application |
| Rust executable process E2E | Human encrypted chat/file workflows across process restarts, persistent session revocation, user/tenant isolation, explicit migration/startup behavior and a 16-peer WebSocket SIGTERM drain | Unix process tests with synthetic accounts and independent Rust clients; no released app or sustained-load qualification |
| PostgreSQL resource tests | Reaction ownership/tombstones, polls, pins, read markers, directory tenancy/revisions, atomic member replacement, voluntary leave, avatar authorization/persistence and live-location state; concurrent reversed-order location batches exercise stable locks | Does not validate desktop/mobile presentation or every callback/event |

The external SDK and application bundle are not distributed with this repository.
See [SDK observations](sdk-compatibility.md) and
[desktop wire observations](client-contract.md) for reference versions and
evidence boundaries.

## Implemented surface

Paths below are relative to `/v20`, except WebSockets and health endpoints. Trailing-slash variants
are accepted by the HTTP adapter.

| Area | Implemented operations | Verification and remaining limits |
| --- | --- | --- |
| Discovery and health | `GET /api_versions`, `/health/live`, `/health/ready` | API 20 discovery; readiness checks database access |
| Human authentication | `POST /u/sessions`, `POST /u/login/{session}`, `POST /u/logout` | Independent ECDH vector and actual HTTP login/logout tests; one-hour PostgreSQL sessions with hashed tokens and master-key-encrypted transport secrets, shared across instances and revoked durably on logout; actual native password login, session survival across own server restart and UI logout followed by 403 on the same captured session passed; app-process restoration and renewal remain unqualified |
| Browser transport | Unauthenticated CORS preflights, configurable literal `Origin: null`, optional exact HTTP(S) web-origin allowlist | Real HTTP tests cover origin/method/header restrictions, protected data still requiring authentication, exposed ETag and no credential/wildcard CORS. The successful native probe sent no HTTP Origin header and WebSocket `Origin: file://`; no native HTTP preflight was observed |
| Bot authentication | `From` App ID with configured transport secret | SDK protocol probes and integration requests; provisioning through the operator CLI |
| Identity and profiles | `GET /u/name`, `GET /u/{id}/info`, `GET /u/profile/{id}`, own profile update | Tenant-limited reads and persisted own profile; full profile-policy qualification remains open |
| Avatars | Persisted user/chat avatar service and storage; user/chat upload, download and deletion adapters | Own-user or chat-administrator writes, tenant/member reads, 2 MiB limit, PNG/JPEG/GIF header and dimension checks, authenticated encryption at rest; actual SDK avatar workflow qualification is tracked separately |
| Settings and preferences | `/u/appsettings`, `GET/PUT /u/settings`, `/u/preferences`, favorite/unread/muted/archived chat mutations | Persisted preferences; advertised unsupported calling/account features are disabled |
| Contacts and search | `GET/POST /contacts`, `POST /u/query` | Authoritative contacts snapshot; user, organisation and managed-circle shapes observed with SDK; directory deletion-event synchronization is incomplete |
| Circles and roles | `GET /g/{id}`, `GET /directory/roles`, `POST /directory/roles/matching_ids` | Tenant-scoped normalized members; operator provisioning; full role/circle join/leave and directory event semantics remain open |
| Directory resync | `GET /directory/sync` | Durable tenant revision with `force_resync.roles` marker requests an authoritative roles snapshot; external SDK fixture confirms refetch behavior; no incremental event history is fabricated |
| Conversation creation | `PUT /t` | Group and one-to-one records, conversation secrets encrypted at rest, initial ACL and transactional outbox; group creation through the actual native human-contact selector passed |
| Subscriptions | `GET /u/subscriptions`, `POST /u/subscriptions/matching_ids` | Conversation key and membership components; SDK response-shape probes and actual native bootstrap/chat selection. Legacy `chat_v`/`chatV` is protocol mode 0, with internal revision kept separate; selected live delivery/history passed, complete resynchronization remains unqualified |
| Membership and administrators | `PUT /t/{id}/members`, `/t/{id}/admin`, `DELETE /t/{id}/subscription` | Version checks, tenant checks, last-administrator protection, atomic ACL/component replacement; group/role membership is materialized when the conversation is created or replaced |
| Conversation settings | `PUT /t/{id}/settings`, `PUT /t/{id}/subscription` | Persisted settings and preferences; not every original-server option has equivalent server enforcement |
| Message posting and deletion | `POST /t`, `DELETE /t/{tid}/m/{mid}` | Opaque conversation-encrypted body, client-ID replay protection, sender checks, deletion tombstones and transactional events; actual native composer text was independently read and decrypted by a second human driver |
| Message synchronization | `GET /feed`, `GET /t/{id}` | Durable global feed cursor; per-conversation ETag is a lower time bound, observed in desktop `getThreadMessages(threadId, daysBack)`; actual native incoming text and renderer reload/history passed after the protocol-mode correction; large-history and full resync qualification remain open |
| Read receipts | `GET/POST /t/{id}/readers`, feed `read_threads` | Persisted read markers and SDK UUID-list response shape; all multi-device ordering and callback combinations remain unqualified |
| Files and binary assets | Multipart `POST /upload`, raw `GET /serve/{id}` | Encrypted bytes remain opaque; private until authorized sharing, ACL checked on download, forwarding references checked, bounded uploads and digest verification. One 69-byte text file passed actual native chooser/upload/display and independent peer download/decryption to identical bytes |
| Reactions | Global, chat and message reaction GETs; POST/PUT/DELETE reaction mutations | Owner-only mutation, current chat membership, retained `Unreact` records and atomic events; complete incremental paging is not implemented |
| Attention acknowledgements | `POST /t/{tid}/reactions/m/{mid}` with `reaction`, GET on the same path, PUT/DELETE by reaction ID | Persisted owning-user records; SDK create/read shapes observed; the unmodified SDK's update helper has a local attribute error described below |
| Polls | `PUT /interactive`, `GET /interactive/{id}`, `POST /interactive` for vote/end | Persisted choices/votes, private before authorized sharing, one vote per person, expiry and creator-only ending; vote/end realtime notifications remain open |
| Pinned messages | `GET/POST /t/{id}/pinnedmessages`, DELETE by message ID | Current membership, same-chat message validation, five active pins, idempotent retries, deleted-message filtering and atomic pin events |
| Live locations | Initial `liveLocation` message attachment; `GET /t/{id}/loc`, `GET /t/{id}/loc/{mid}`, `GET /t/loc`, `POST /u/loc`, `DELETE /t/{id}/loc` | Persisted encrypted coordinates, owner-only update/stop, current membership, expiry and atomic updates; an external SDK probe accepts a single-record `live_location` callback; complete native map/reconnect/background tracking workflows remain unqualified |
| WebSockets | `/ws/v20/` with `From`; `/u/ws/auth` and `/ws/v20/{token}` | Binary SDK frames and text human-client frames, URL-safe single-use tickets, subscriptions, heartbeat, durable catch-up; actual native live incoming text and server-restart reconnect passed; complete callback/reconnect equivalence remains unqualified |
| Desktop notification selection | `PUT /notifications/token` with literal `pnstype: "ws"` | Real encrypted HTTP tests and actual desktop login accept the `devicetoken` body; APNs, FCM, unknown and missing transport types return 501; external push is not implemented |
| Typing and audio-recording presence | Authorized chat broadcasts over WebSocket | Process-local ephemeral delivery; no cluster-wide presence broker |

### Data limits and synchronization

Resource snapshots containing more than 1,000 directory entries, reactions,
active live locations or read-thread entries fail explicitly. A truncated result must not be treated as a
successful full synchronization. Reaction queries can narrow `from` and `to`
using ISO timestamps. Cursor paging for these resources remains a compatibility
gap; reducing a query window is not a replacement for qualifying native resync.

Circle and role memberships contribute to conversation access through a stored
snapshot of their current normalized users. Later directory changes do not yet
recompute every affected conversation or emit all vendor-equivalent directory
events. A production deployment requiring dynamic managed-group authorization
needs that propagation before qualification.

Directory revisions advance transactionally on user/profile and directory
changes. They are logical timestamp watermarks with millisecond precision, not
wall-clock audit timestamps. The API captures a revision before reading a
snapshot. A concurrent change can therefore cause a repeated snapshot rather
than be skipped. Role synchronization uses an explicit full-resync marker;
native incremental user/circle deletions and all directory event callbacks still
require qualification.

Poll sharing currently requires the message author to be the poll creator.
Forwarding another person's poll is rejected. The implementation deliberately
checks referenced resource access before committing the containing message.
Binary assets have separately verified authorized forwarding semantics.

Voluntary conversation leave is separate from administrator-driven member
replacement. A member can unsubscribe without administrative privileges. The
last administrator must appoint another administrator while other members
remain; a sole remaining user can leave. Member replacement retains its acting
administrator implicitly, matching the observed SDK mutation behavior.

Live-location durations are limited to 1 second–31 days, and encrypted coordinate
strings to 64 KiB. Coordinates remain opaque to this feature. Batch updates lock
conversations in a stable order and commit all updates/events together. Expired
or deleted shares cannot be updated, and leaving or removal revokes reads.

### Observed SDK limitation

An external SDK probe created and read an attention-message reaction with the
documented fields. The resulting SDK `AMReaction` stores `reaction_id`, while
`Chat.update_am_reaction` accesses `.id` before sending its HTTP request. In the
unmodified tested wheel this raises `AttributeError`. A server response
containing both ID fields did not create the missing SDK attribute. The server
update route exists, but successful use of that specific SDK helper cannot be
claimed from a patched client or a hand-written HTTP request.

## Work still required for normal Teamwire applications

| Area | Required qualification or implementation |
| --- | --- |
| Desktop 5.0.1 | Actual login, contact selection, group creation, outgoing/incoming text, one file upload/display, renderer reload/history, server-restart session survival and UI logout are recorded. Remaining: pristine installation, app-process restart/relogin, native one-to-one flows, reactions, polls, avatars, native read-receipt semantics, large-history repair/resync and other permission/connection-recovery combinations |
| Android and iOS | Capture the exact supported versions' public client/server contract, then run the same real-client workflows; no mobile version is currently qualified |
| Registration and recovery | Organization discovery, self-registration/invitations, email verification, password change/reset, account recovery and disabled-user behavior |
| Session management | Native session listing/closure, device management, session renewal and all native restart/multi-device expiry cases; durable server authentication records are implemented |
| PIN and device security | PIN creation/verification/reset, registration tokens, required device/audit information and local-wipe responses |
| SSO | Exact OIDC/SAML/other supported client redirect, token exchange, refresh and logout contracts; no SSO flow is implemented or qualified |
| Directory lifecycle | Incremental event history, deleted users/groups/roles, role joins/leaves, circle changes, dynamic ACL propagation and native full-resync semantics |
| Native push | Persisted device registration and real APNs/FCM delivery, background wake-up, token rotation and privacy settings; WebSocket delivery does not establish push compatibility |
| Calls | Real Jitsi deployment/integration, room authorization, call action events, media admission, one-to-one/group audio/video and reconnection; no media workflow is currently qualified |
| Advanced content | Actual avatar resizing and live-location map/background workflows, SOS/PTT, calendar/role-rich attachments, lists and every original permission combination |
| Federation and administration | Federation trust/routing, cross-organization membership, management UI/API, retention, exports and administrative account lifecycle |
| Operation | An isolated synthetic logical dump/asset restore passed; production backup/PITR, upgrade qualification, sustained load and memory measurements, multi-instance session/presence behavior and fault recovery remain open |

Unknown operations return an explicit error. A server must not advertise an
unsupported feature solely because a matching URL has been added. In
particular, accepting a notification-token request does not establish external
push delivery.

## Reproducing the checks

Use a disposable PostgreSQL database. Tests create unique fixtures and must not
run against a production database.

```sh
cargo test -p hoowire-crypto
HOOWIRE_TEST_DATABASE_URL='<disposable PostgreSQL URL>' \
  cargo test -p hoowire-db --test resources -- --ignored
HOOWIRE_TEST_DATABASE_URL='<disposable PostgreSQL URL>' \
  cargo test -p hoowire-api --test interoperability -- --ignored
HOOWIRE_TEST_DATABASE_URL='<disposable PostgreSQL URL>' \
  cargo test -p hoowire-api --test cors -- --ignored
```

The selected native scenarios and disclosed isolation methodology are recorded in
the local, unpublished `artifacts/native-desktop-probe.json` and
[the desktop contract](client-contract.md). Generated reports are excluded from
the public source checkout. The native app, vendor source,
credentials and private raw traces are not redistributed.

Run the external SDK suite separately as described in
[SDK compatibility evidence](sdk-compatibility.md). Keep its generated JSON
report with the tested HooWire revision and SDK version. Releasing a compatibility
claim requires passing every declared scenario and recording the native-client
versions and actual workflows that were exercised.

No throughput, latency or resident-memory target has yet been established by a
published benchmark. The Rust runtime, bounded concurrency, pooled database
connections and optimized release profile are implementation choices, not
performance measurements.
