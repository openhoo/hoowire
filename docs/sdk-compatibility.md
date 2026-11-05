# SDK compatibility evidence

## Reference client

The initial compatibility target is the official **Teamwire Python SDK 2.0.0**
for Python 3.11, 3.12 and 3.13, obtained from the public download page on
2026-11-04. Its migration guide identifies **Backend API 20** as the transport
version. SDK 1.7 and 1.8 use different notification semantics and transport.

Reference archive SHA-256:

```text
866df082b99651c360fb94d3125a273aa08ca93915c0479b92a6707c21bde0a4
```

The separately hashed Python 3.11 wheel used in the later qualification is
`teamwire_api-2.0.0-py311-none-any.whl`, SHA-256
`86297e08e0d1c34fcf30c7eda5c7a08be44cf8e93e62875dee8ee448914d100c`.
The archive and wheel are different files and have different digests.

The SDK is externally licensed and is not bundled in this repository. Test
operators obtain and license their own copy. HooWire is an independent project;
the name Teamwire is used to identify the intended interoperability target.

## Evidence boundaries

Public SDK documentation describes Python objects, methods and callback events.
It does not provide a complete HTTP or WebSocket protocol specification.
Additional observations were made by executing the **unmodified SDK** against
an ephemeral loopback HTTP/WebSocket recorder with dummy credentials and dummy
data. No production Teamwire system was contacted for these tests. No SDK
source was decompiled or copied into HooWire.

The executable suite is `tests/sdk_conformance.py`. Its method inventory in
`tests/sdk_surface.json` lists 84 documented methods, including client-local
helpers and constructors. Every report separates successful scenarios, failed
scenarios, blocked fixture-dependent scenarios, attempted methods, successfully
qualified methods, and methods not exercised.
Passing a selected scenario is evidence for that scenario only.

## Observed transport

| Area | Observation |
| --- | --- |
| API prefix | `/v20` |
| Bot identity | `From` HTTP header contains the App ID |
| Client identifier | `User-Agent: teamwire/SDK_2.0.0` for HTTP requests |
| Body envelope | Base64 of `IV[16] || SHA256(IV)[32] || AES-256-CBC(PKCS#7(plaintext))` |
| Envelope key | The 32 ASCII bytes of the API secret |
| Message text | Independently encrypted with the chat `col` secret, inside the outer API envelope |
| WebSocket | `/ws/v20/`, `From` header, binary frames containing the Base64 envelope |
| Subscription | Decrypted first client frame: `{"action":"subscribe","message_types":["chat_message","chat_update"]}` |
| Message timestamp | `ts` in milliseconds since Unix epoch; fixture `1700000000000` becomes `2023-11-14T22:13:20Z` in the SDK |
| Per-chat feed | A raw JSON list of message records |
| Global feed | Object with a `messages` list |
| Subscriptions | A raw JSON list of `{tid,col,members,threadInfo}` records |
| Matching subscriptions | Object containing a `subscriptions` list |
| Direct members | Top-level `members: {users:[],groups:[],roles:[]}` on a subscription |
| Membership / leave | Bodyless HTTP 204 on success; membership replacement retains the acting user |
| Read receipts | `{"readers":["user UUID",...]}`; a list of reader objects is rejected by the SDK |
| Post response | `timestamp` is in seconds; message records continue to use `ts` in milliseconds |
| Asset upload | Multipart field `f`, filename `f`; binary file parts contain the raw envelope encrypted with the chat `col` |
| Asset upload response | Plain JSON with `url: "asset:///UUID"`; ordinary API JSON responses remain encrypted |
| Bot avatar | PNG uploaded as a plaintext multipart part to `/u/profile/{uid}/image/upload` |
| Chat avatar | JPEG uploaded as a plaintext multipart part to `/t/{tid}/avatar` |
| Avatar download | Raw binary envelope encrypted for the requesting session, including chat avatars |
| Directory sync | `force_resync: {roles: revision_milliseconds}` triggers a full SDK role refresh when newer than its cursor |
| Message callback | Binary frame with `{"type":"chat_message","content": message_record}`; verified with a different sending user |
| Live location | Message field `liveLocation` contains a JSON string with encrypted `{lat,lng}` coordinates and a duration |
| Location state | `activeLiveLocations` count; per-message state includes `location`, `active`, and `lastActive` in milliseconds |
| Live subscription | Client sends `{type:"live_location",action:"subscribe",tid}`; server sends `{type:"live_location",content: location_record}` |
| Attention reactions | Client subscribes to `chat_reaction`; events contain an ACK record with `id`, `mid`, `tid`, `userId`, and `reaction` |

These observations establish encodings and shapes for the tested client. They
do not establish every error code, concurrency rule, permission edge case,
reconnection sequence, attachment format, event callback or server feature.
In particular, complete compatibility additionally requires reconnect tests,
complete role/circle event semantics, permission combinations and older-version
qualification. An inventory of method names is not an inventory of all input
combinations or all Teamwire enterprise features.

## Reference SDK defect

The unmodified SDK 2.0.0 parses a successful attention reaction into an
`AMReaction` whose public identifier is `reaction_id`. Calling the documented
`Chat.update_am_reaction(reaction, ReactionType.SOS)` with that object raises:

```text
AttributeError: 'AMReaction' object has no attribute 'id'
```

The failure occurs inside the reference client before it sends an HTTP request.
Supplying both `id` and `reaction_id` in the server response does not add an `id`
attribute to the parsed SDK object. The suite records this failure explicitly;
it does not modify or monkey-patch the SDK to obtain a green report. Successful
reaction creation, listing, and callbacks remain independently testable.

## Recorded integration run after shared tickets

The qualification at **2026-11-05 06:21:06 UTC**
(**2026-11-05 07:21:06 Europe/Berlin**) used the unmodified SDK 2.0.0 on
Python 3.11.15 and the frozen server source containing migration `0010` and
PostgreSQL-backed WebSocket tickets. It used a fresh disposable PostgreSQL
17.11 database with authenticated TCP readiness, SCRAM-SHA-256 host rules and
rejection of a wrong password. All optional scenarios and `--require-complete`
were enabled:

- **111 scenarios passed**, **one failed** and **one dependent readback was
  blocked**. All **84 methods were attempted**, with **83 qualified**.
- The only failed method remains `Chat.update_am_reaction`, which raises the
  reference client's `AMReaction.id` attribute error before sending HTTP. The
  suite preserved its strict exit code **1**.
- The source/fixture manifest contained **78 files**. That manifest, the private
  executable copy, all **32 installed SDK RECORD hashes**, and all **28 hashed
  original files in the supplied wheel** matched before and after execution.

The external SDK's declared dependency environment, including
`cryptography==48.0.0`, was kept unchanged. The independent recovery driver was
subsequently qualified in a separate environment with `cryptography==50.0.0`;
neither Python environment is a dependency of the Rust server runtime.

```text
source_sha256: 5bfdfb8a2ce102834812d2aa1403ede070f9c2bf43cf84c3210013b99e467e0b
binary_sha256: 552289f5dce21cb61f1207fb9521cd6aa0cb934d9702c261769f8e160232b63e
```

The separate authenticated aiohttp transport probe received HTTP **101**, the
encrypted subscription acknowledgement and authenticated HTTP **200** before
sending SIGTERM. It received **Close 1001** with reason `Server shutdown`,
explicitly confirmed the peer close reply with automatic closing disabled,
and observed server exit **0** in **0.002194 seconds**, within eight seconds.
This probe is separate from executing SDK methods and from native app evidence.

Local ignored reports are `artifacts/sdk-conformance-smoke-20261105.json`,
`sdk-source-smoke-20261105.json`, `sdk-shutdown-smoke-20261105.json` and
`sdk-cleanup-smoke-20261105.json`. The owned server, database container, private
binary, fixture metadata and credentials were removed afterward. Earlier
reports were preserved. This qualification covers Python 3.11 and the selected
SDK scenarios; it does not renew the earlier desktop probe or qualify mobile,
Python 3.12/3.13, older SDKs or complete Teamwire compatibility.

## Earlier local integration run

`artifacts/sdk-conformance-final.json` records the **2026-11-04 23:16:13 UTC**
run (**2026-11-05 00:16:13 Europe/Berlin**) using the unmodified SDK 2.0.0 on
Python 3.11.15 against the local Rust server. This build includes migration
0009, durable human sessions, URL-safe WebSocket tickets, native-client
conversation status, separation of chat protocol mode from the internal
optimistic revision, HTTP feed ETag expiry, and graceful WebSocket shutdown.
The run used a fresh disposable PostgreSQL database, newly provisioned tenant,
two bots, one human account, a circle and a role. PostgreSQL readiness required
an authenticated TCP `SELECT 1`; all host authentication rules used
SCRAM-SHA-256 and a wrong password was rejected. All **32 recorded file hashes**
of the installed external SDK distribution were verified before and after
execution. All optional scenarios and the strict `--require-complete` gate were
enabled:

- **111 scenarios passed**, one scenario failed, and one dependent readback
  scenario was blocked by that failure.
- All **84 inventoried methods were attempted**; **83 methods qualified** in
  the selected scenarios. No method was silently omitted.
- The only failed method is the reference SDK's `Chat.update_am_reaction`
  attribute error described above. The dependent updated-reaction readback is
  recorded separately as blocked. The strict SDK suite exits **1**, so this run
  does not claim complete compatibility.
- Incoming text callbacks from a second SDK bot, the deprecated notification
  entry point, live location callbacks, attention reaction callbacks, and their
  unsubscribe methods all passed against HooWire.
- PNG/JPEG avatars, file forwarding, image/audio/video attachments and poll
  state were read back through SDK objects with explicit content assertions.

The report includes an operator-supplied SHA-256 digest of the source and test
fixtures associated with that build. It is a source snapshot identifier, not a
remote binary attestation or a complete compatibility certification. The
reproducible path list, individual file hashes and digest algorithm are in
`artifacts/sdk-source-final.json`. `tests/source_manifest.py` records Rust source,
migrations, Cargo manifests/lockfile, SDK test drivers, method inventory and the
synthetic image/audio/video fixtures. The **72-file source manifest** was
unchanged before and after both the SDK run and shutdown probe:

```text
source_sha256: d8fe4652d00b97c0100a15898430f1cb8fdee891e331838363b37619b1ab3e7a
binary_sha256: 3d876bf8a9635fd1c0eb1653eb72beab44f441a8c18e0c93f2c320b6cd00ba0a
```

The tested executable was copied to a private fixture directory before the
parallel workspace test gates started. Only that frozen copy was launched;
its SHA-256 was verified before and after qualification. The reports record its
temporary path and distinguish the copy from the workspace build output, which
may be relinked by parallel Cargo gates. The fixture directory, copied binary,
credentials, owned server process and PostgreSQL container were removed after
the run.

`artifacts/sdk-shutdown-smoke.json` and the identical
`artifacts/shutdown-smoke.json` record that earlier independent aiohttp transport
probe against that same frozen executable. It completed HTTP **101**, received
the authenticated subscription acknowledgement, and verified authenticated
HTTP **200** immediately before sending SIGTERM to its owned process. The
client received **Close 1001** with reason `Server shutdown`, explicitly sent a
Close reply with automatic closing disabled, and observed server exit **0** in
**0.002107 seconds**, within an eight-second deadline.

Historical `artifacts/shutdown-observation.json` and
`artifacts/shutdown-diagnosis.json` remain unchanged and belong to the earlier
62-file source snapshot (`e3210b845dccfb4b70c3d89b28a451cec30dcc55e769442093c35f188c3bd04a`)
and executable (`880e4104573b07d201c9a2b90706ee84f436e1eec6d985921fb997163b7ba6e3`).
The original probe observed a terminal client state before recording Close;
bounded follow-up probes with an empty outbox and 64 queued messages later
received Close on four peers and observed successful process exits. Those
historical artifacts are not evidence for the later build, and the later
success does not erase the initial observation. Transport probes are separate
from native application evidence. Python 3.12/3.13 and older SDKs were not
executed in this run.

## Running the SDK suite

Use a disposable HooWire database with an operator-created bot and a second
fixture user. Install the external SDK wheel using Python of the matching minor
version and the dependencies declared by that wheel.

After freezing the source used for the server build, create its source/fixture
manifest. Supply the resulting `source_sha256` field to the suite through
`HOOWIRE_TEST_SERVER_SOURCE_SHA256` or `--server-source-sha256`. Recreate both the
manifest and SDK report after changes to included source or fixture files.

```sh
python tests/source_manifest.py --output target/sdk-source.json
```

```sh
export TW_API_HOST=http://127.0.0.1:8080
export TW_API_APPID='<fixture App ID>'
export TW_API_SECRET='<fixture API secret>'
export HOOWIRE_TEST_OTHER_USER_ID='<second fixture user UUID>'
export HOOWIRE_TEST_SENDER_APPID='<second fixture bot App ID>'
export HOOWIRE_TEST_SENDER_SECRET='<second fixture bot API secret>'
export HOOWIRE_TEST_SENDER_USER_ID='<second fixture bot user UUID>'
python tests/sdk_conformance.py \
  --extended --avatars --reactions --live-location --notifications \
  --require-complete \
  --report target/sdk-conformance.json
```

The suite mutates fixture data through the SDK. It checks SDK object readback,
Unicode message preservation, timestamp conversion, deletion, settings/profile
persistence, membership, administrators and attachments. The extended scenarios
round-trip synthetic image, sound, video and file bytes, forward a received
attachment, and read the poll question and choices back through SDK objects.
Avatar scenarios verify unchanged image bytes. Callback scenarios use a second
disposable bot for incoming messages and bounded fresh SDK connections for live
location and attention reactions. Both the current notification entry point and
the deprecated `run_task` alias are exercised. Without a circle or a
role in the fixture, those scenarios are recorded as blocked. Optional media
and notification scenarios that are not exercised remain visible in the report.
`--require-complete` also fails the run when any inventoried method is missing
from successful scenarios. This prevents a partial suite from being reported
as full compatibility.

## Public reference material

- SDK download: <https://sdk.teamwire.eu/download/>
- SDK reference: <https://sdk.teamwire.eu/api/2.0/connection/>
- API 20 migration and notification transport: <https://sdk.teamwire.eu/api_guide/migration_2.0/>
- Chat methods: <https://sdk.teamwire.eu/api/2.0/chats/>
- Attachment methods: <https://sdk.teamwire.eu/api/2.0/attachments/>
- Roles: <https://sdk.teamwire.eu/api/2.0/roles/>
- External SDK terms: <https://sdk.teamwire.eu/license/>
