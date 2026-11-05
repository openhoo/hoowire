# syntax=docker/dockerfile:1
FROM rust:1.93.1-slim-bookworm AS build
ENV RUSTUP_TOOLCHAIN=1.93.1
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN --mount=type=cache,id=hoowire-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=hoowire-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=hoowire-target,target=/build/target,sharing=locked \
    cargo build --locked --release --bin hoowire \
    && cp target/release/hoowire /usr/local/bin/hoowire

FROM debian:bookworm-slim AS runtime
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN groupadd --gid 10001 hoowire \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin hoowire \
    && install -d -m 0700 -o 10001 -g 10001 /data/assets
COPY --from=build /usr/local/bin/hoowire /usr/local/bin/hoowire
COPY LICENSE NOTICE /usr/share/doc/hoowire/
USER 10001:10001
WORKDIR /data
ENV HOOWIRE_BIND=0.0.0.0:8080 HOOWIRE_DATA_DIR=/data/assets
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/hoowire"]
CMD ["serve"]
