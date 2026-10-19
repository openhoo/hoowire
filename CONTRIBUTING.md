# Contributing

Use Node.js 22+ and Python 3.9+. Full tests require macOS. Run the three commands
in the README's development section before proposing a release.

- Keep Teamwire binaries, extracted bundles, app backups, profiles, tokens,
  certificates and real chat screenshots out of commits and release files.
- Use synthetic fixtures to describe composer changes. Do not evaluate vendor
  JavaScript or download code to execute during discovery.
- Reject ambiguous or unsupported structures. Keep attachment policies, sender
  checks, media URL validation, size limits and cancellation intact.
- Preserve hardened runtime and existing entitlements. Sign only a separate copy
  and verify it before installation. Keep rollback and recovery working.
- A fixture passing with a changed version number is not a new compatibility
  claim. Record real integration results in `docs/compatibility.md`, including
  what was not tested. Do not send chat messages as part of automated tests.
- Bump `RECIPE_VERSION` for a changed transformation and `VERSION` for a release.
  Run packaging from a clean checkout and verify downloaded release checksums.

Patcher releases contain source only. The package script's explicit file list
must remain narrow; add new public files deliberately.
