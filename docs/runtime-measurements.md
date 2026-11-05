# Runtime measurements

Reviewed November 5, 2026. These observations describe one local Linux ARM64
release artifact and one idle container sample. Throughput, latency, sustained
load and memory growth with connected clients remain unmeasured.

## Current artifact and environment

The current image qualification passed on November 5, 2026 in Europe/Berlin.
Its recorded UTC timestamps fall on November 4, 2026 because the run occurred
after midnight locally. The image contains the explicit WebSocket-task drain;
the image smoke used an idle server with no active WebSockets or SDK clients.
Active-client shutdown evidence belongs to the separate process and SDK checks
described in [testing](testing.md).

| Item | Observed value |
| --- | --- |
| HooWire version | `0.1.0` |
| Local image tag | `hoowire:local` |
| Build-input snapshot | `2026-11-04T23:14:53.439794+00:00` |
| Inputs verified after build | `2026-11-04T23:16:24.185506+00:00` |
| Inputs verified after image tests | `2026-11-04T23:17:30.582697+00:00` |
| Image check started | `2026-11-04T23:16:50Z` |
| Target | `linux/arm64`, GNU libc |
| Rust toolchain | `1.93.1` |
| Build profile | Release, thin LTO, one code generation unit, stripped symbols, abort on panic |
| Runtime base | Debian Bookworm slim |
| Docker server | `29.8.2` |
| Docker Compose | `5.5.0` |
| Linux VM | OrbStack, kernel `7.2.12-orbstack-00394-gbecdd6130068`, `aarch64` |
| VM resources | 16 CPUs, 16,818,020,352 bytes of memory |
| Desktop host | macOS `26.6.2`, build `25G83` |
| Test database | Separate disposable `postgres:17.11-alpine` container |

Artifact identity:

```text
image_id=sha256:580a49e086f75358c76e1075400a13dacdae2251f133d3a8406c4c424ce94423
linux_release_binary_sha256=167e5c39740c366002f9240932d6a7be61093fa71852441c091eb68a82a8a10f
source_freeze_sha256=d8fe4652d00b97c0100a15898430f1cb8fdee891e331838363b37619b1ab3e7a
build_inputs_sha256=436cc7e38fbedf00c937154a75265f5cd2e304605809dedb7c13d04e2c7aa36a
copy_inputs_sha256=4bff652dce38561fd3ebb21bafe80d5b56f847b215d42bbf9b352b6e7693e38f
cargo_lock_sha256=28c6f5513851fac28bed2f6210116d15013c2daffc14e846514cc00d87877ee0
image_check_script_sha256=6c3d97662c3bbdf5f4b1bdbd47498e13f3d07af7196c8c7c19d951d14549c794
builder_base=rust:1.93.1-slim-bookworm@sha256:5b9332190bb3b9ece73b810cd1f1e9f06343b294ce184bcb067f0747d7d333ea
runtime_base=debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3
builder_arm64_manifest=sha256:c4aff1f5fe1b20c1a2652776235f4d81ee041a722045b771634361ff4c933ba3
runtime_arm64_manifest=sha256:7ccb8b5b60c7654f5b795d94ee7d751cd0811500803f0b09cefcd555e755a55a
```

The source-freeze digest covers the 72-file Rust, test and SDK-fixture inventory
defined by [source_manifest.py](../tests/source_manifest.py). It matched the
frozen inventory before the image build, after the build and after the smoke.

The build-input digest has a separate scope: **67 files**, including
`Dockerfile` and `.dockerignore` as build controls. Of these, **65 files** are
actual Docker `COPY` inputs: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`,
`LICENSE`, `NOTICE` and every file under `crates/`. The `copy_inputs_sha256`
value covers just those 65 files. For both input digests, files are sorted by
repository-relative POSIX path; each UTF-8 path, a NUL byte, its contents and
another NUL byte are appended to SHA-256.

Every individual build-input digest and the separate image-check script digest
matched after the build and after the smoke. The image ID and extracted Linux
binary digest matched before and after the smoke. Registry base-index digests
also matched before and after, and the BuildKit provenance records the same
base identities.

The local, unpublished directory
`artifacts/image-qualification/2026-11-05-580a49e086f7/` retains the input
manifest, source freeze, environment, BuildKit metadata, build and smoke logs,
before/after observations, summary and checksums. Generated artifacts are
excluded from the public source checkout.

## Size and idle observation

| Observation | Result |
| --- | --- |
| Executable size inside image | **5,906,512 bytes** / **5.63 MiB** |
| Docker image size, uncompressed | **103,386,578 bytes** / **98.60 MiB** |
| Single Docker memory-usage display | `1.168MiB / 15.66GiB` |
| Single Docker CPU display | `0.00%` |
| Single Docker PIDs display | `18` |
| Idle SIGTERM | Exit `0` within the configured 40-second grace |

MiB uses 1,048,576 bytes. The image size includes the Debian runtime filesystem
and executable. Registry transfer size depends on layer compression.

The idle observation came from `docker stats --no-stream`, requested after a
five-second wait following successful health requests. The server had no SDK
clients, WebSockets, active user sessions, messages or conversations. Its
database contained the default tenant plus one fixture tenant, one user and a
group and role used by the provisioning checks. PostgreSQL ran in a separate
container. No explicit CPU or memory limit was applied to the server container.

Record the memory value as this Docker container-memory sample. Process RSS,
loaded sessions, concurrent requests, uploads and large replay batches require
their own measurements. The displayed CPU percentage is one idle observation.

## Image checks performed

The image ran as UID/GID `10001:10001`, with a read-only root filesystem,
`cap-drop=ALL`, `no-new-privileges` and a writable temporary asset mount. The
check used its own internal Docker network and PostgreSQL container.
The fresh database initialized host authentication with SCRAM-SHA-256.

The following checks passed:

- CLI version and master-key generation with networking disabled.
- Migration help exposes the separate 300-second default budget; invalid CLI
  budgets of zero and 3,601 seconds are rejected before database access.
- CA bundle present; Node.js and Python absent from the runtime executable path.
- Dynamic libraries resolved: GNU libc, libm, libgcc and the ELF loader.
- Explicit migrations using a separate owner role without superuser privileges.
- Startup with the documented runtime grants, read-only migration metadata and
  no schema creation privilege.
- `create-tenant`, `create-user` and both directory kinds, including complete
  membership replacement with `--id`.
- HTTP `/health/live` and `/health/ready` returned `200`.
- Idle SIGTERM shutdown returned exit code `0` within the configured 40-second
  grace, with no active WebSockets.
- All containers and the network owned by the check were removed afterward.

Database TLS was disabled within this isolated test network. Production TLS,
recovery and client qualification follow the separate
[operations](operations.md) and [compatibility](compatibility.md) procedures.
The smoke does not establish production readiness or complete SDK compatibility.

## Historical image observation

The earlier qualification on November 4, 2026 is retained in the local,
unpublished `artifacts/image-qualification/2026-11-04-05f7130f20af/` directory.
The current artifact directory also retains the preceding version of this
document as `previous-runtime-measurements.md`.

```text
image_id=sha256:05f7130f20af0eb4817f28876380247910ccaa1950aaf0bae1ae85dcafc330d1
linux_release_binary_sha256=1be1e2af4c6fddec21305ce37eabd792726140401bb5cee922b75852c62e6417
build_inputs_sha256=70a1c6535056913bda3263cc0878c7a2a57281fbbcdc18ad8f4720ecca1b965a
cargo_lock_sha256=9951980867c58b692b2e5e080a16872ce96d7c11c858b7b744413fccee1b0414
image_check_script_sha256=9d1e72ae80a4cb458ac1c0e60bd9b28cd92a8088216a98744e2b49c47aecfd9f
```

Its 57 build inputs used the same digest format. The snapshot was recorded at
`2026-11-04T22:35:24.909087Z`; source equality was checked after the build at
`2026-11-04T22:37:03.479956Z` and after the smoke at
`2026-11-04T22:37:43.685326Z`. Its smoke began at `2026-11-04T22:37:07Z`.
It used the same toolchain, base digests and host environment and its own
PostgreSQL 17.11 container.

| Historical observation | Result |
| --- | --- |
| Executable size | 5,906,512 bytes / 5.63 MiB |
| Uncompressed image size | 103,386,578 bytes / 98.60 MiB |
| Single Docker memory display | `1.395MiB / 15.66GiB` |
| Single Docker CPU / PIDs display | `0.00%` / `18` |
| Idle SIGTERM | Exit `0` within the configured **35-second** grace |

That image predates the explicit WebSocket-task drain. Its idle check had no
connected WebSockets. These historical values qualify only the older image and
binary listed above. Equal artifact sizes do not imply equal binaries. The two
single idle memory displays are not a regression benchmark.

## Repeat the size checks

```sh
docker build --tag hoowire:local .
./scripts/test-image.sh hoowire:local
docker image inspect hoowire:local --format '{{.Id}} {{.Size}} {{.Os}}/{{.Architecture}}'
docker run --rm --network none --read-only --entrypoint /bin/sh hoowire:local \
  -c 'wc -c < /usr/local/bin/hoowire'
```

The [image test script](../scripts/test-image.sh) reproduces the role,
provisioning, read-only serving and idle shutdown checks with its own resources.
Its readiness check uses an authenticated TCP query against the actual target
database, avoiding the temporary socket-only server during PostgreSQL image
initialization.

For an isolated running instance, capture one idle sample with
`docker stats --no-stream <container-id>` and record its fixture state and host
environment. Base-image updates, another target architecture, compiler changes
and application revisions can change these observations.
