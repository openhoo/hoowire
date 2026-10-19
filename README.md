# Hoowire

Hoowire is an OpenHoo tool that adds a **GIF button** next to Teamwire's emoji button on
macOS and Windows. Search GIPHY, browse animated previews, and select a GIF to add it as a
normal draft attachment. Use Teamwire's **Send** button when ready.

The automatic patcher reapplies the extension after compatible Teamwire updates.
It waits for Teamwire to close, keeps an original backup, checks the app's
structure, prepares a separate copy, and verifies the installation. macOS copies
are signed locally; supported Windows executables keep their vendor signature.

**Experimental, unofficial desktop modification.** Verified with Teamwire 5.0.1
on an Apple Silicon Mac. Windows support targets the official x64 desktop MSI
and is checked by native Windows CI. Other versions are subject to structural compatibility
checks; passing those checks is not a promise of future compatibility.

## Requirements

- macOS or Windows 10/11 with an existing Teamwire desktop installation.
- Node.js 22 or newer and Python 3.9 or newer.
- **macOS:** The background agent uses
  `/usr/bin/python3`; make sure that executable works before enabling it.
- **macOS:** Apple's command line developer tools (`codesign`, `security`, `ditto`, and
  `plutil`) and a usable **Apple Development** signing certificate with its private
  key in the local keychain. You can manage development certificates in Xcode.
- **Windows:** A standard Python installation with `pythonw.exe`, Windows
  PowerShell 5.1, and Task Scheduler. No Apple tools or local signing certificate
  are required. Use Teamwire's per-user install and a normal terminal: the
  background task runs with your normal user permissions.
- Write access to the Teamwire installation directory and enough disk space for
  several complete app copies. Backups are retained without automatic pruning.
- Network access to GIPHY for search, previews, and the selected GIF.

Acorn is vendored with its license and recorded file hashes. There is no npm
install step. Teamwire binaries, signing keys, and account data are not included.

## Install

Download and extract an archive from
[Releases](https://github.com/openhoo/hoowire/releases), then open a
terminal in the extracted directory. Alternatively:

```sh
git clone https://github.com/openhoo/hoowire.git
cd hoowire
```

### Windows

Quit Teamwire, open a normal PowerShell terminal in the extracted Hoowire folder,
and run:

```powershell
py -3 scripts/patcher.py enable
py -3 scripts/patcher.py apply
py -3 scripts/patcher.py status
```

Hoowire searches the usual per-user and Program Files locations. If Teamwire is
installed elsewhere or several copies are found, select its installation folder
or executable explicitly:

```powershell
py -3 scripts/patcher.py enable --app "C:\Users\You\AppData\Local\Programs\Teamwire\Teamwire.exe"
py -3 scripts/patcher.py apply
```

The app directory must be writable by your normal user so the scheduled task can
replace it. For a protected, machine-wide installation, use Teamwire's per-user
installation instead. Keep the state directory and app on the same drive for
atomic recovery. Windows settings, runtime, logs and backups live in
`%LOCALAPPDATA%\Hoowire`. Use the same `py -3 scripts/patcher.py` prefix with the
`check`, `disable`, `status`, or `apply --rebuild` commands below.

### macOS

Quit Teamwire, then enable the automatic patcher and apply the first patch:

```sh
python3 scripts/patcher.py enable
python3 scripts/patcher.py apply
python3 scripts/patcher.py status
```

If exactly one Apple Development identity is available, it is selected
automatically. For multiple identities or a nonstandard app/Node location:

```sh
python3 scripts/patcher.py enable \
  --app "$HOME/Applications/Teamwire.app" \
  --node /absolute/path/to/node \
  --identity "Apple Development: Your Name (TEAMID)"
python3 scripts/patcher.py apply
```

The app and Node paths are saved by `enable`. Subsequent `apply` and `check`
commands use that saved configuration. `status` uses the configured app unless
you explicitly pass `--app`. Avoid `sudo`: the agent belongs to your login session
and uses your local signing identity.

Open Teamwire, click **GIF** beside the emoji button, search, and select an image.
The picker follows Teamwire's existing file attachment restrictions. Selection
adds an attachment to the current draft; it does not send a message.

## Automatic operation

`enable` installs a standalone runtime and a per-user background agent.

On macOS, Hoowire retains the original service identifier and data folder so existing
installations continue to use the same configuration and backups.

| Item | Location |
| --- | --- |
| Agent | `~/Library/LaunchAgents/eu.openhoo.teamwire-gif-patcher.plist` |
| Runtime, configuration, status and logs | `~/Library/Application Support/Teamwire GIF Patcher/` |
| Original and replaced app copies | `~/Library/Application Support/Teamwire GIF Patcher/backups/` |

On Windows the agent is a Task Scheduler task under `OpenHoo`, named
`Hoowire-<your user SID>`. It runs at login and every two minutes, using
`pythonw.exe` to avoid opening a console. It uses your normal interactive login
and stops running when you log out. `disable` unregisters this user's task.

The agent checks at login and every 120 seconds; macOS also watches app resource
changes. It waits at least 15 seconds for a changed bundle to settle and never
quits or launches Teamwire. An unsupported structure or missing signer produces
a `blocked` status and a retry after 15 minutes, or sooner when the app changes.
Use `apply` to force a new check after correcting a problem.

```sh
python3 scripts/patcher.py status
python3 scripts/patcher.py check
```

The downloaded source directory can be moved after enabling: the agent uses its
own installed copy. To update the patcher itself, download a newer release and
run `enable` from that release, followed by `apply`. There is no network
self-update of patch recipes. To rebuild an installed patch from its retained
vendor backup:

```sh
python3 scripts/patcher.py apply --rebuild
```

## Disable and restore

Stop automatic patching:

```sh
python3 scripts/patcher.py disable
```

This keeps both the installed GIF extension and the backups. To restore the
original Teamwire app, disable the agent, quit Teamwire, and find `original_backup`
in `state.json` or the `vendor-<sha256>.app` (macOS) / `vendor-<sha256>` (Windows)
directory for the current version.
Restore that explicit backup with:

```sh
python3 scripts/install.py \
  --source "/absolute/path/to/vendor-<sha256>.app" \
  --destination /Applications/Teamwire.app
```

The installer verifies the backup's signature and retains the app it replaces.
On Windows the equivalent command is:

```powershell
py -3 scripts/patcher.py disable
# Quit Teamwire, then use the original_backup path recorded in state.json:
py -3 scripts/install.py --source "C:\path\to\vendor-<sha256>" --destination "C:\path\to\Teamwire"
```

For a custom app location, pass that location as `--destination`. Restoring an
older backup also restores that older Teamwire version. Installing a fresh
official Teamwire build is another way to remove the extension after disabling.
Keep backups until you have verified the restored app. If you already have an
older local patch, its matching original backup must be present before enabling.

## Compatibility and signing

Teamwire has no composer extension hook used by this project. The builder edits
its compiled Electron app archive and updates the bundle's ASAR integrity hash.
Acorn parses the compiled JavaScript without executing it and resolves the
composer's actual minified bindings. Missing or ambiguous anchors, unsupported
entry points, and changes to the checked Electron preload/security configuration
are rejected before app replacement.

On macOS, the patched copy and its native binaries are signed consistently with your local
Apple Development identity. Existing entitlements, signing flags, and hardened
runtime metadata are preserved. This changes the app's signing identity and does
not retain the vendor's notarization. A managed-device policy may therefore
reject the modified app. Signature verification, a recovery journal, retained
copies, and rollback protect installation; they cannot establish compatibility
with every future Teamwire update.

On Windows, Hoowire checks the executable's Authenticode signature and supported
Teamwire publisher. It modifies only `resources/app.asar`, leaving the executable,
its signature, and Electron fuses unchanged. Supported builds have embedded ASAR
validation disabled by the vendor. A future build with that validation enabled,
or an unknown fuse format, is rejected before writing; Hoowire does not disable
the protection or invalidate the executable signature. This limitation is
checked independently of the composer checks. See [verified scope](docs/compatibility.md).

## GIPHY and privacy

The provider reads JSON embedded in public GIPHY search pages. **It does not use
the official GIPHY API and requires no API key.** GIPHY page changes, network
restrictions, or service limits can break search independently of Teamwire.

Search terms, your network address, preview requests, and the chosen GIF request
reach GIPHY. The provider's search and download requests omit credentials and
referrers and reject redirects. Previews load from GIPHY's media CDN inside the
app. This extension does not send message text, recipient information, or
Teamwire credentials to GIPHY. Teamwire handles an attached GIF through its normal
attachment workflow.

Only HTTPS `media*.giphy.com/media/` URLs are accepted. Downloads must correspond
to a recent result for the requesting window, have a GIF signature, and fit the
20 MiB limit. Search responses are limited to 5 MiB and provider requests time out
after 15 seconds. Closing the picker cancels pending provider work. Results have
no additional content filtering in this patcher.

## Development and releases

```sh
node --test test/provider.test.cjs test/transform.test.cjs
python3 -m unittest discover -s test -p 'test_*.py' -v
python3 scripts/package.py
```

The regression suite runs on macOS and Windows with synthetic app fixtures.
The Node tests are portable. Tests run offline and do not send messages or use a
real Teamwire profile. Packaging verifies the vendored parser and creates
deterministic source archives plus `SHA256SUMS` under `dist/`. Only explicit
source and documentation paths can enter a release.

The manually dispatched `windows-vendor` CI option additionally downloads the
pinned official MSI, verifies its checksum and publisher signature, extracts a
disposable copy, and exercises patching, the real renderer and preload bridge,
the scheduled runtime, and restore. Its profile is empty; it never authenticates
or sends a chat message. GIPHY availability is reported separately because a
datacenter network may be unable to access that service.

Read [CONTRIBUTING.md](CONTRIBUTING.md) before changing the patch recipe and
[SECURITY.md](SECURITY.md) for reporting security issues.

## License

Apache-2.0 for OpenHoo's code; Acorn is MIT licensed. See [LICENSE](LICENSE),
[NOTICE](NOTICE), and [vendor/acorn/LICENSE](vendor/acorn/LICENSE).
Teamwire and GIPHY belong to their respective owners. This project is independent
and is not endorsed by either company.
