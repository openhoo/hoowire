# Verified scope

## Hoowire 1.1.0: Windows support

Windows uses the same composer transformation, picker and preload bridge as
macOS, with a Windows layout adapter, Authenticode verification, process checks,
native file locks, and a per-user scheduled task. Windows app copies retain the
original executable and all fuse values. The official Windows 5.0.1 x64 MSI was
inspected during development; its embedded ASAR validation fuse is disabled.
The renderer and composer bindings were successfully discovered in its actual
archive. An enabled or unknown integrity configuration is rejected before writes.

The pinned vendor installer SHA-256 is
`0ce660fde43034eac59604eee3b63dbac480bc2087b8d729cd9a93b6590271b3`.
It is fetched only by the opt-in Windows integration job and is not redistributed.

Native Windows qualification is pending the integration CI run. The normal suite
adds Windows fixture, lock, rollback, crash recovery and signature rejection
coverage. The real renderer probe uses an empty profile and an isolated file
callback; it does not qualify authenticated composer use or recipient delivery.
External GIPHY search/selection results are recorded separately in the CI summary.

## Hoowire 1.0.1

Version 1.0.1 names the project Hoowire and updates its repository links, release
archive names, notices, and CLI description. The patch recipe and GIF picker are
unchanged from 1.0.0. Existing service identifiers and data paths are retained so
installed agents keep using their configuration and backups. The offline suite
is run for the renamed release; the real-app integration below was performed for
1.0.0.

## Initial release: 1.0.0

Local integration was verified on macOS on Apple Silicon with:

| Component | Version |
| --- | --- |
| Teamwire desktop | 5.0.1 |
| Bundled Electron | 38.0.0 |
| Bundled React | 19.2.1 |
| Local Python | 3.9.6 |
| Local Node.js | 26.12.0 |

The signed integration cycle exercised a clean vendor app, waiting for app exit,
backup creation, structural discovery, building, signing all native binaries and
the bundle, installation, signature verification, and a second unchanged pass.
The installed app was launched with its existing local account, GIF search
returned animated results, and selecting one staged a GIF attachment. The test
attachment was removed. No chat message was sent during verification.

Offline regression tests cover provider parsing and URL constraints, request
cancellation and download limits, renamed minified bindings, attachment policy
gating, composer discovery, missing or ambiguous anchors, changed security
configuration, manifest integrity, update stability and app-exit deferral,
backup preservation, a missing signer, installation races, rollback, interrupted
installation recovery, and status for a custom app location.

Tests using a synthetic future version and renamed renderer asset show that the
builder discovers those fields dynamically. They are not evidence that another
real Teamwire release works. Intel macOS, other Teamwire builds, other operating
systems, managed-device policies, and actual recipient-side delivery have not
been qualified by this release.

## Architecture

1. `build.py` reads the ASAR archive and bundle metadata and resolves its renderer.
2. `transform.cjs` uses the vendored Acorn parser to locate and modify the composer
   with the bindings found in that exact app. Vendor code is parsed, never run.
3. `src/picker.js` provides the button's search/select interface. The selected
   file goes through Teamwire's existing attachment callback.
4. `src/preload.cjs` exposes search, download and cancel operations. `src/main.cjs`
   checks the requesting Electron frame and delegates to `provider.cjs`.
5. `sign.py` signs macOS app copies with the configured local identity, or verifies
   the unchanged vendor executable on Windows.
6. `install.py` verifies and replaces the app with retained backups and recovery.
7. `patcher.py` runs these steps through a bounded, locked background pass.

The public source and archives include no Teamwire application or extracted
vendor implementation. Test fixtures are synthetic. Local backups, logs, signing
identity configuration and verification screenshots are private to the user.
