# Security

HooWire 0.1.0 is early software. The compatibility reports describe its verified
scope. No independent security audit or production qualification is claimed.

Report a suspected vulnerability privately to the repository maintainers using
GitHub private vulnerability reporting when it is enabled. If that facility is
unavailable, ask a maintainer for a private reporting channel without posting
the exploit, credentials or affected users' data in a public issue.

Include the affected revision, a minimal reproduction with synthetic data and
the expected authorization boundary. Exclude passwords, master keys, integration
secrets, database URLs, real messages and vendor binaries.

Operational boundaries:

- Terminate HTTPS and WSS at a trusted reverse proxy; the application serves HTTP.
- Use a dedicated PostgreSQL runtime role and verified database TLS outside the
  local development network. Apply migrations through a separate owner role.
- Back up the master key, PostgreSQL and immutable asset storage together. The
  master key is required to open stored conversation and integration keys.
- Restrict local provisioning commands and secret files to trusted operators.
- Legacy SDK transport encoding exists for interoperability. It is not a
  substitute for TLS or an assertion about the SDK's end-to-end security.

See [operations](docs/operations.md) and [data integrity](docs/data.md) for the
concrete deployment and recovery procedures.
