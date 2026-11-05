# Desktop client contract and native verification

This is a factual interoperability record from static inspection of Teamwire
desktop **5.0.1**, extracted on 19 October 2026, and an isolated macOS desktop
probe on **4 November 2026**. It is not a vendor specification or an
interoperability certification. No vendor implementation code is included in
HooWire. Mobile clients have not been traced.

## Evidence

The local, unredistributed evidence is
`teamwire-analysis-20261019/extracted/build/assets/ModalManager-B_ejrVRQ.js`,
SHA-256 `f326d7c0a12922a25bedaad86a994b301e20f1719785dcebf667c2be550ce69c`.
Offsets below are character offsets in that UTF-8 decoded file, not byte offsets.
The source manifest identifies the application as Teamwire 5.0.1.

Live-location consumer observations additionally use
`teamwire-analysis-20261019/extracted/build/assets/index-DzVYt-79.js`, SHA-256
`920a056aee5c0f1e307bead66858cdf4583f242f87cdfab77991820066b3a047`.

| Evidence region | Observed behavior |
| --- | --- |
| 458326–475750 | secp256k1, transport key derivation, AES-CBC envelopes |
| 475800–477450 | API base, headers, response decoding, error statuses |
| 477450–492800 | Assets, polls, messages, subscriptions, contacts, preferences |
| 497500–500750 | WebSocket authentication, frames, subscriptions, heartbeat |
| 513750–517950 | Human authentication, registration, settings, calls, directory |
| 838100–845800 | Post-login initialisation and required response fields |
| 999700–1013000 | Feed, incremental contacts, system messages and synchronisation |
| 1034000–1067500 | Nested message encryption, attachment encryption, typing |

## Transport and session authentication

The client sets its HTTP base to `https://{host}/v20` and WebSocket base to
`wss://{host}/ws/v20/`. Authenticated HTTP requests carry `From: {from_id}` and
`X-Request-ID: {uuid}`. The public session identifier and the internal owner user
identifier are distinct values. JSON mutations generally use
`Content-Type: application/octet-stream` with a base64 text body.

1. `POST /u/sessions/` accepts plain JSON `{client_pubkey}`. The public key is
   uncompressed SEC1 secp256k1 encoded as hex. The plain response contains
   `{server_pubkey,session_id}`.
2. The shared ECDH point is encoded as uncompressed SEC1. Bytes 1 through 24
   inclusive are base64 encoded. The resulting 32 ASCII bytes are the AES key.
   Equivalently, use the first 24 bytes of the shared point's X coordinate.
3. `POST /u/login/{session_id}/` accepts encrypted JSON with
   `{email_hash,password,registration_token?,audit_info?}`. `email_hash` is the
   lowercase hexadecimal SHA-256 of the lowercased email's UTF-8 bytes.
4. The encrypted login response must supply `from_id`, `owner_id`, `owner_type`,
   `organisation_id`, `first_name`, `last_name`, and `send_contacts` for the
   initialisation code. There may be further server fields.

An encrypted envelope is `IV[16] || SHA256(IV)[32] || AES-CBC-PKCS7(ciphertext)`.
JSON HTTP bodies and WebSocket frames base64 encode these bytes. Assets transport
the raw bytes. This legacy format has no ciphertext authentication tag. Its
SHA-256 prefix is an IV checksum and does not authenticate message contents.
HTTPS and WSS remain required for normal clients.

Normal responses recursively convert snake_case field names to camelCase in the
client. HooWire should return the observed wire names.

Plain error responses are inspected before decryption: HTTP 403 with
`{error:"not authorized..."}` or `{error:"user/connector disabled"}` expires the
session; 419 requests password reset; 451 requests local wipe.

## Initialisation and incremental synchronisation

All paths in this section are under `/v20`. Responses are session encrypted
unless explicitly described as plain.

| Method and path | Request / response facts |
| --- | --- |
| `GET /api_versions/?format=json` | Plain `{versions:["20.0",...]}`; strings are parsed as floats. |
| `GET /u/appsettings` | A flat object containing both permission and setting fields. The client splits these into two local objects. |
| `GET /u/settings?platform=desktop` | Settings object; `autosave` and `read_receipts` become local autosaveMedia/sendReadReceipts. Web uses platform `webclient`. |
| `PUT /u/settings?platform=desktop` | Object with one changed setting key. No response decoding. |
| `PUT /notifications/token` | App/device registration; see below. No response decoding. |
| `POST /contacts` | Full `{users,organisations,managed_groups,force_resync}`. |
| `GET /contacts` | Incremental `{users,organisations,managed_groups,deleted_users,deleted_groups,force_resync}` and ETag. |
| `GET /u/subscriptions/?legacy_circle_ids=true` | Array of subscriptions and ETag. |
| `POST /u/subscriptions/matching_ids?legacy_circle_ids=true` | `{thread_ids:[...]}` -> `{subscriptions:[...]}`. |
| `GET /u/preferences/` | `{favourite_chats:[],unread_chats:[],muted_chats:[],archived_chats:[]}`. |
| `GET /feed` | `{messages:[...],read_threads:[{tid,last_read_ts}]}` and ETag; `last_read_ts` is an ISO timestamp. 304 has no body. |
| `GET /u/reactions` | `{reactions:[...]}` with ETag; supports 304. |
| `GET /u/lists/?legacy_circle_ids=true` | `{lists:[...]}` and ETag. |
| `GET /directory/roles` | `{roles:[...]}` and ETag. |
| `GET /directory/sync` | `{events:[...],force_resync:...}` and ETag; supports 304. |

ETag here is the **hex encoding of an ISO timestamp's ASCII bytes**, not a normal
quoted HTTP entity tag. The client sends the same format in a request `ETag`
header. Some initialisation paths unconditionally decode the response ETag, so
omitting it breaks those paths. This behavior is directly observed; exact server
cursor tie-breaking and timestamp precision still require live comparison.

`GET /t/{tid}` uses a lower history bound: `getThreadMessages(threadId,
daysBack)` computes an ISO timestamp `daysBack` days before the current time and
sends it as ETag (observation at character offset 483668). The call site at
998959 uses the same days-back range as the global feed. No upper/before history
cursor was observed in this desktop version.

Directory sync's `force_resync` is an object. The roles synchronizer checks
`force_resync.roles > millis(lastRolesSync)` and, when true, requests a complete
`GET /directory/roles` snapshot. Its event parser otherwise handles
`role.created`, `role.updated`, `role.visibility.added`,
`role.visibility.removed`, `roles.created`, `roles.deleted` and
`roles.joined.updated` (observations at character offsets 992400–994700).

User entries consumed during bootstrap include
`{id,first_name,last_name,type,organisation,has_avatar,avatar_modified_at,federation_name,profile}`.
`profile` includes `account_email`, `about_me`, `department`, `job_title`, `phone`
and related editable contact properties. Organisation entries include
`{id,name,federation_name}`. `managed_groups` IDs are normalised to strings by the
client. Changing a nonempty `force_resync` token causes a full resynchronisation.

The desktop notification registration body contains
`{appid:"desktop",config:{disable_silent_push:true,enable_v18_notifications:true,enable_v19_reactions:true},devicetoken,info:{brand,client:"Desktop",client_v,is_managed,locale,model,os,os_v},pnstype:"ws",version}`.

The known app-setting fields, expressed as their client property names, are:
`appColor`, `logo`, `logoUpdatedAt`, `lowerCaseRequired`,
`maxAllowedPinnedMessagesPerChat`, `maxChatMembers`, `messagesRetentionPeriod`,
`minLengthRequired`, `numberRequired`, `passwordDuration`, `pinInterval`,
`platform`, `requirePin`, `rolesEnabled`, `rolesSettings`, `showContactData`,
`symbolRequired`, and `upperCaseRequired`. Known permission properties are
`allowAccessCalendar`, `allowAccessCamera`, `allowAccessLocation`,
`allowAccessMicrophone`, `allowAccessPhotos`, `allowAccessVideos`,
`allowAttachments`, `allowAutoSaveMedia`, `allowBlockingUsers`,
`allowChangePassword`, `allowCloseChats`, `allowCloseSessions`, `allowCopyPaste`,
`allowDeleteMessages`, `allowDisableReadReceipts`, `allowEmailConversation`,
`allowFileSharing`, `allowInteractiveAttachments`, `allowInviteMembers`,
`allowLeaveConversation`, `allowLiveLocation`, `allowNotificationPreview`,
`allowOpenIn`, `allowPushToTalk`, `allowReplyFromNotifications`, `allowVoipCalls`,
`allowVoipGroupCalls`, `arcgis`, `enableAlerting`, `enableEditLoginPassword`,
`enableEditUserProfileDepartment`, `enableEditUserProfileEmail`,
`enableEditUserProfileNotes`, `enableEditUserProfilePhone`,
`enableEditUserProfileRole`, `enableEditUserProfileStatus`,
`enableEditUserProfileWebsite`, `enableSOSButton`, and `maxGroupCallMembers`.
These names are an allowlist of what the observed app consumes, not proof that
each corresponding server setting has been implemented. Wire spelling for
acronyms such as SOS still needs dynamic verification.

The app's local user-setting defaults use tone `default`, enabled autosave,
enabled read receipts, send with Enter, enabled sending sound, and enabled
startup. The actual platform-specific operating system setting must remain a
client decision.

## Chats, messages and assets

A subscription has `{tid,col,members,thread_info,ts,chat_v?}`. Members contain
`{users,groups?,roles?}`. `thread_info` contains `type` (`1to1` or `group`), `title`,
`admins`, `allow_mute_notifications`, `allow_sending`, `avatar_modified_at`,
`has_avatar`, `is_closed`, `is_federated`, `is_public`, and `status`. Local active
conversations use `status: "readwrite"`.

The desktop treats `chat_v`/`chatV` as a **protocol format mode**, not an internal
chat change counter. Its legacy-mode predicate is `Fk = e => (e ?? 0) <= 0`.
Full normal-message synchronization includes only subscriptions passing that
predicate; sidebar last-message metadata updates independently. Returning an
internal database revision of 1 therefore let the sidebar decrypt incoming text
while suppressing normal-message storage/presentation. HooWire emits mode 0 and
keeps its internal optimistic revision separate as `version`. Native replay with
a new incoming message established the correction.

The client creates a fresh `col` as base64(random 24 bytes). Message text,
location, calendar, roles and attachment contents are encrypted with this **chat
key** before the request object is encrypted with the session key. HooWire must
preserve these opaque fields. In particular, message request `msg` becomes
message record `m`. Sending encrypted text as ordinary plaintext to another
client cannot establish compatibility.

| Method and path | Request / response facts |
| --- | --- |
| `PUT /t/?legacy_circle_ids=true` | `{chatType,users,admins?,allowMuteNotifications?,allowSending?,title?,col,groups?,roles?,avatar?}` -> subscription. Creation keys deliberately include observed camelCase fields. |
| `PUT /t/{tid}/members/?legacy_circle_ids=true` | `{groups,roles,users}`; no response decoding. |
| `PUT /t/{tid}/subscription` | Per-subscription options; no response decoding. |
| `DELETE /t/{tid}/subscription` | Leave/delete own subscription; no response decoding. |
| `PUT /t/{tid}/settings` | Changed chat settings such as `title`, `isClosed`; no response decoding. |
| `POST /t` | `{cid,msg,notification,tid,isAlert,alertType?,reply_to?,gallery?,forwarded?,files?,snd?,interactive?,video?,location?,calendar?,mentions?,mapOverlay?,roles?}` -> `{tid,mid,ts}`. |
| `GET /t/{tid}/` | Message array; request ETag selects history cutoff. Supports 304. |
| `DELETE /t/{tid}/m/{mid}` | Delete message; no response decoding. |
| `GET /t/{tid}/readers` | Reader data; live server schema not fully established. |
| `POST /t/{tid}/readers` | `{mid,mts}` -> `{readers,ts}` or 204. |
| `PUT`, `DELETE /t/{tid}/admin` | `{user}`; no response decoding. |
| `POST /upload` | Multipart field `f`, raw chat-encrypted bytes -> plain `{id}`. |
| `GET /serve/{id}` | Raw chat-encrypted asset bytes. |
| `GET`, `POST`, `DELETE /t/{tid}/avatar` | GET returns encrypted bytes; POST is multipart field `f`; DELETE has no decoded response. |
| `GET /t/{tid}/pinnedmessages` | `{pinnedmessages:[{mid,ts}]}`; this field intentionally has no underscore. |
| `POST /t/{tid}/pinnedmessages` | `{message_id}`; no response decoding. |
| `DELETE /t/{tid}/pinnedmessages/{mid}` | No response decoding. |
| `GET /t/{tid}/loc/`, `GET /t/{tid}/loc/{mid}`, `GET /t/loc` | Live-location retrieval; the SDK and desktop consumers require different field spellings, described below. |

Message records include `mid`, `tid`, `from`, `m`, `ts`, and attachment/system
fields. Normal message timestamps are numerical; feed read timestamps are ISO.
Gallery is a JSON string of `asset:///id` references. Audio `snd`, video, and map
overlay use `asset:///id` strings. Files and interactive attachments are JSON
strings. Client file entries include `{id,origin:"asset",v:"2",type,extension,mimeType,size,name}`.

### Polls and reactions

| Method and path | Request / response facts |
| --- | --- |
| `PUT /interactive` | `{type:"poll",data:{...}}` -> poll record. |
| `GET /interactive/{id}` | Poll record including `id`, `choices:[{id,count,...}]`, `my_choice`, `has_ended`. |
| `POST /interactive` | Vote `{id,response:{choice},type:"poll_response"}` or end `{id,type:"poll_end"}` -> poll record. |
| `GET /t/{tid}/reactions?from=...&to=...` | `{reactions:[...]}`. |
| `GET /t/{tid}/reactions/m/{mid}` | `{reactions:[...]}`. |
| `POST /t/{tid}/m/{mid}/reactions` | `{emoji}` -> reaction record. |
| `PUT /t/{tid}/m/{mid}/reactions/{id}` | `{emoji}` -> reaction record. |
| `DELETE /t/{tid}/m/{mid}/reactions/{id}` | Reaction record. |

Reaction records contain `id`, `mid`, `tid`, `ts`, `user_id`, `emoji`, `action`.
`action:"React"` adds a reaction; other actions remove it. The client converts
`mid` to a string and normalises skin tone modifiers for grouping.

### Live locations

The desktop location viewer calls `GET /t/{tid}/loc` and consumes wire
`{active_live_locations,live_location:[...]}` after snake-to-camel conversion.
Records include `id`, `tid`, `user`, encrypted `location`, optional
`map_overlay`, and `active`. `GET /t/{tid}/loc/{mid}` supplies `active` and
`last_active` for an ended share. These consumer observations are at character
offsets 100930 and 552533 in the additional bundle listed above.

The unmodified Python SDK 2.0.0 separately accepts the camel spelling
`activeLiveLocations` for the count and requires `liveLocation` on a message to
construct its location-sharing attachment. Its observed location update body is
`{liveLocation:[{tid,location}]}` at `POST /u/loc`. It stops its own sharing with
`DELETE /t/{tid}/loc`. A single-record SDK response consumes `lastActive` in
milliseconds; the desktop consumes `last_active` after its normal field-name
conversion. An independent timestamp fixture establishes the millisecond unit.
These spellings must be retained where a shared wire adapter supports both
clients. Initial message attachments carry a JSON string in `liveLocation`,
containing `{duration,location}`; the coordinates themselves remain encrypted
with the conversation key.

## WebSocket

`GET /u/ws/auth` returns encrypted `{token}`. The client connects to
`wss://{host}/ws/v20/{token}`. Frames are base64 session-encrypted JSON.

The default subscription sent after connection is
`{action:"subscribe",message_types:["chat_message","chat_preferences","chat_update","notification","voip_notification","reaction"]}`.
Incoming frames have `{type,content}`. For `chat_message`, `content` is the full
message record, including nested chat-encrypted `m`; it is not just a feed hint.
Other observed types are `live_location`, `live_location_tracking`, `broadcast`.

The unmodified Python SDK's live-location topic frames are
`{type:"live_location",action:"subscribe"|"unsubscribe",tid}`. A decrypted
server frame `{type:"live_location",content:record}`, where `record` has the
single-share retrieval shape, reaches its real `LiveLocationUpdate` handler in
an external protocol probe. This establishes SDK callback parsing, not an actual
desktop map or background tracking workflow. The SDK sends its encrypted frames
as WebSocket binary messages.

The client sends `{type:"ping"}` every 15 seconds and expects a message within
10 seconds. A `pong` response is sufficient for this observed heartbeat path.

Typing and audio recording use topic subscriptions
`{action:"subscribe"|"unsubscribe",tid,type:"broadcast"}` and messages
`{action:"send",event:"user_typing"|"user_recording_audio",tid,type:"broadcast"}`.
Exact incoming broadcast content fields require dynamic verification.

System messages have `from:"sys"` and observed `type` values `chat.created`,
`chat.members.added`, `chat.members.removed`, `chats.join`, `chats.leave`,
`mt_update`, `mt_delete`, `msg_pin`, `msg_unpin`, `circles.deleted`,
`circle.users.added`, `circle.users.removed`, `roles.deleted`, `roles.joined`,
`roles.left`, `role.users.removed`, `task.state`, `call_action`.

## Additional account surface

Observed requests include:

- `POST /u/logout`;
- `GET /u/profile/{id}`, `POST /u/profile/{id}` (profile updates use snake_case);
- `GET /u/{id}/info`;
- `GET /u/profile/{id}/image`, multipart `POST /u/profile/{id}/image/upload`,
  `DELETE /u/profile/{id}/image`;
- `GET /u/sessions`, `DELETE /u/sessions/{id}`, `DELETE /u/sessions` (other sessions);
- `GET /u/block`, `PUT /u/block/` `{user}`, `DELETE /u/block/{id}`;
- `GET /u/logo/` encrypted binary;
- `POST /u/query?match_department&match_role` `{query_string}` -> `{results}`;
- `POST /u/favouritechats/`, `/u/unreadchats/`, `/u/archivedchats/`,
  `/u/mutedchats/` with `{tid}`, and DELETE of the corresponding `/{tid}/`;
- `PATCH /u/unreadchats/` with `{action:"clear"|"set",threads:[...]}`;
- `POST /u/lists/?legacy_circle_ids=true`, `PUT /u/lists/{id}?legacy_circle_ids=true`
  with list fields and `groups`; DELETE `/u/lists/{id}/`;
- `PUT /u/roles` with `{roles}`;
- `POST /u/password` requests password change;
- `PUT /u/password` `{new_password,pin}`;
- `PUT /u/password/reset` `{oldPassword,password1,password2}`;
- plain `POST /u/register/{sha256(phone)}` with
  `{email:base64(lowercase_email),first_name,last_name,nmbr:base64(phone),udid,registration_token}`
  -> `{pin}`;
- plain `POST /u/signin/{sha256(phone)}/{sha256(udid)}` with `{pin,col:client_pubkey}`
  -> plain `{token:server_pubkey,data:encrypted_user_metadata}`.

Registration and PIN lifecycle were only observed statically. A production
mail/SMS verification flow, invitation policy, anti-abuse controls and recovery
semantics have not been established by these observations.

## Isolated native desktop probe — 4 November 2026

A real Teamwire desktop **5.0.1** macOS arm64 application, using Electron
**38.0.0**, was launched against a disposable loopback HooWire backend. The
source was an original vendor backup, separate from the installed application
and its Teamwire Patcher extension. Source ASAR size was **216,339,964 bytes**,
SHA-256 `bc523d053f8ee6216cdcde41b2c8922d1af203461e4d5630f4b06fa092dab029`.
The source archive was rehashed and remained unchanged. The installed app and
its existing profile were not used for this probe.

This was a disclosed isolation build. Exactly two ASAR paths changed:
`build/electron/main.js` gained an early isolation-loader import, and
`build/electron/hoowire-isolation.cjs` was added. No renderer, client protocol,
message parser, encryption function or response handler was changed. The
result qualifies only these recorded scenarios with that isolation setup; it
does not establish pristine-install or mobile compatibility. Digests, outcomes
and sanitized request evidence are recorded in the local, unpublished
`artifacts/native-desktop-probe.json`. This generated report is excluded from
the public source checkout.

The final native scenarios used debug executable SHA-256
`880e4104573b07d201c9a2b90706ee84f436e1eec6d985921fb997163b7ba6e3`,
associated with source/fixture snapshot
`e3210b845dccfb4b70c3d89b28a451cec30dcc55e769442093c35f188c3bd04a`.
That snapshot is retained locally under
`artifacts/historical/e3210b845dcc/source-freeze-final.json`. A later change
adds an explicit bounded WebSocket shutdown drain and Close 1001 before runtime
exit, covered by a separate real-process SIGTERM regression test. The native
scenarios were not repeated after that lifecycle change; this report records
the exact earlier executable and does not attest a later native run.

### Isolation and TLS

Isolation was applied before vendor imports cached paths or requested a
single-instance lock:

| Surface | Probe setup |
| --- | --- |
| Bundle identity | Separate application name, main bundle ID and four helper IDs; every copied Mach-O and the copied bundle signed with an existing local Apple Development identity; `codesign --verify --deep --strict` passed |
| Files and profile | Fresh private temporary root; separate Electron `home`, `appData`, `userData`, `sessionData`, `temp`, logs, crash dumps, disk/media caches and launch `TMPDIR`; no real database or account copied |
| Keychain | Keytar replaced by an in-memory map; no real Keychain item created, read or deleted |
| Login items | `auto-launch` calls replaced by no-op adapters; the hard-coded vendor login-item name otherwise addresses the real Teamwire installation |
| External effects | External navigation and `shell.openExternal` rejected; native updater adapters disabled; DNS and Node socket guards plus Chromium session guards restrict requests to the chosen loopback TLS backend and local resources |
| Chromium sandbox | Vendor Chromium sandbox retained. An extra outer `sandbox-exec` wrapper was tested separately but conflicted with Chromium's nested sandbox and single-instance Unix socket; it was not used for the successful UI run |
| Infrastructure | Own PostgreSQL 17.11 container with disposable tmpfs, separate database and assets; HTTP `127.0.0.1:18081` behind HTTPS/WSS gateway `127.0.0.1:18443`; no production backend used |

The original macOS configuration supports `serverHost`, `email`,
`trustedCertificates`, `hideAppUpdateNotification` and `runOnStartup`. The
probe selected the explicit loopback server, avoiding email-domain discovery.
Its HTTPS certificate used an IP subject alternative name. The unmodified
vendor certificate handler accepted its exact SHA-256 fingerprint through the
managed configuration's `trustedCertificates` allowlist. No system-wide trust
anchor, global certificate bypass or `--no-sandbox` flag was used.

The actual native **HTTP requests had no Origin header**. The actual native
**WebSocket upgrades sent `Origin: file://`**. No native HTTP preflight was
captured. A previous static preparation hypothesis predicted opaque
`Origin: null`; this run did not confirm that hypothesis. Independent HTTP
CORS tests establish the explicitly configured null-origin policy below.

### Recorded native scenarios

The two human accounts and all messages/files were synthetic. The user
explicitly approved accepting the application terms for this isolated test
copy. Account credentials, From tokens, transport/chat keys, raw vendor files
and unsanitized logs are excluded from the repository artifact.

| Scenario | Evidence and result |
| --- | --- |
| Password login | Actual UI submitted secp256k1 session handshake and encrypted login; both returned 200; local encrypted SQLite initialized and UI showed the own synthetic identity |
| Initial bootstrap | Contacts, subscriptions, preferences, app settings, user settings, directory sync and WebSocket token requests returned 200; empty account reached a usable landing screen |
| Human contacts and chat creation | A second provisioned synthetic human appeared in the actual native contact selector; native group chat creation returned 200 and the conversation appeared in the sidebar |
| Server restart/session survival | The native account remained authenticated after its own HooWire process was stopped and restarted against the same disposable database/master key; new WS authentication and upgrade succeeded |
| Native outgoing text | Actual composer submitted `POST /t` with encrypted text; response 200; a separate human HTTP driver read the persisted record and decrypted its inner `m` to exactly the typed UTF-8 text |
| Renderer reload/history | Native renderer reload retained login and showed the existing chat and outgoing text; a separate HTTP history read independently established that the server held the message |
| Native file attachment | Actual paperclip and macOS file chooser selected a 69-byte synthetic text file; multipart upload and message POST returned 200; native main chat showed `TXT native-attachment.txt 69 B` with timestamp |
| Incoming human message | An independent Python ECDH/HTTP human driver posted nested chat-encrypted text; after correcting the protocol mode, a new message appeared without reload in the open native main chat with the correct peer name and text |
| Incoming history after reload | The new peer message, own text and native file block remained visible after renderer reload and reopening the conversation. Automatic repair of feed positions advanced before the mode correction was not qualified |
| Native logout and revocation | Actual native account-menu logout and confirmation returned 200 and restored the login screen. The same privately captured native From token returned 200 before logout and 403 after logout |

The peer driver was **not a second released native client**. It used its own
human session, verified the native outgoing message through history, posted a
reply using the observed native outer shape, then logged out and received 403
when reusing that revoked session. This distinguishes transport/crypto evidence
from a native-to-native acceptance test.

The run exposed three concrete native blockers already corrected and replayed:
WebSocket authentication tickets must be URL-safe raw path segments, and local
active conversation `thread_info.status` must be `"readwrite"`. Standard
Base64 tickets containing a slash caused intermittent 404 upgrades; a status
of `"active"` made the native composer display a closed/unfederated chat.
The original renderer uses `getChatStatus(tid) !== "readwrite"` to block writing.
The third blocker was the protocol `chat_v` mismatch explained above: after
mode 0 was returned and the renderer reloaded, a newly posted peer message was
correctly displayed live and retained across another renderer reload. No
client parser patch was used to obtain these results.

The optional/background GETs `/u/lists/?legacy_circle_ids=true`, `/u/block`
and `/u/logo/` returned explicit 501 responses. Login, text sending and the
file composer remained usable. Native lists, blocking and company-logo
workflows are therefore not qualified.

After the final UI logout/revocation check, the own native process, TLS gateway,
HooWire process and disposable PostgreSQL container were stopped; all three
test ports were closed. The generated application copy, temporary profile,
raw traces, TLS key, captured session tokens and temporary drivers were
removed. The separate installed Teamwire process remained running. Only the
sanitized report and source/approach digests remain in this repository.

### Browser transport and notification policy

HooWire installs CORS handling around the complete HTTP router. OPTIONS
preflights do not require a `From` identity; protected data requests still do.
The literal `Origin: null` is allowed by default and can be disabled with
`HOOWIRE_CORS_ALLOW_DESKTOP=false`. Separately hosted web clients need exact
serialized origins in comma-separated `HOOWIRE_CORS_WEB_ORIGINS`. Wildcards,
paths, user info, query strings and fragments are rejected. CORS does not
enable cookies or HTTP credential mode.

The method allowlist is GET, HEAD, POST, PUT, PATCH and DELETE. Request headers
include `From`, `Content-Type`, `X-Request-ID` and `ETag`; responses expose
`ETag`. Three real HTTP/PostgreSQL tests passed on 4 November 2026: desktop
preflight plus authenticated encrypted data, exact web-origin/header/method
restriction, and notification transport selection. The CORS tests establish
HTTP policy independently of the native run's absent HTTP Origin header.

Desktop registration using `PUT /notifications/token` succeeds only for
literal `pnstype: "ws"`, matching the native `devicetoken` body. APNs, FCM,
unknown types and missing `pnstype` return HTTP 501. This selects existing
WebSocket delivery; HooWire does not persist or deliver external push tokens.

Authenticated human sessions are stored durably in PostgreSQL for one hour:
From tokens are hashed and transport keys encrypted with the server master
key. Every authentication checks the durable record; logout revokes it and
signals active WebSockets across instances. Native session listing, device
management, renewal, app-process restart and all multi-device expiry behavior
remain unqualified.

## Calls and remaining compatibility boundary

Observed call signaling paths are `POST /calls/` with `{mode,participants,thread}`,
`GET /calls/{uuid}/auth_info/`, `PATCH /calls/{uuid}/` with `{action}`,
`PUT /calls/{uuid}/`, and `GET /calls/?tid={tid}&status=active` -> `{results}`.
The application uses Jitsi React/Electron SDKs for actual calls and screen sharing.
HTTP route coverage cannot substitute for a working Jitsi media deployment,
accepted authentication metadata, client permissions or real peer tests.

These observations establish one desktop build's wire surface. They do not
establish complete mobile behavior, push services, SSO, federation, administration,
all retention/deletion edge cases, or real multi-client interoperability. Keep
those acceptance items visible when reporting compatibility.
