# syntax=docker/dockerfile:1.25.0@sha256:0adf442eae370b6087e08edc7c50b552d80ddf261576f4ebd6421006b2461f12
ARG RUST_BUILDER="docker.io/library/rust:1.85.1-alpine3.21@sha256:4333721398de61f53ccbe53b0b855bcc4bb49e55828e8f652d7a8ac33dd0c118"
ARG GATEWAY_RUNTIME="docker.io/library/alpine:3.21@sha256:48b0309ca019d89d40f670aa1bc06e426dc0931948452e8491e3d65087abc07d"

FROM ${RUST_BUILDER} AS builder
ARG SOURCE_COMMIT
ARG SOURCE_DATE_EPOCH
ENV CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml ./
COPY .cargo .cargo
COPY crates crates
COPY services services
COPY access-gateway access-gateway
COPY xtask xtask
COPY migrations migrations
COPY vendor vendor
RUN --mount=type=cache,id=labweaver-gateway-cargo-target,target=/src/target,sharing=locked \
    test -n "${SOURCE_COMMIT}" && test -n "${SOURCE_DATE_EPOCH}" && \
    cargo build --release --locked --offline --package access-gateway && \
    install -D -m 0555 target/release/access-gateway /out/labweaver-gateway

FROM ${GATEWAY_RUNTIME} AS runtime
ARG SOURCE_COMMIT
RUN apk add --no-cache \
      openssh-client-default=9.9_p2-r0 \
      openssh-server=9.9_p2-r0 \
    && adduser -D -H -s /sbin/nologin gateway-auth \
    && adduser -D -H -s /bin/sh gateway \
    && addgroup gateway gateway-auth \
    && mkdir -p /run/sshd /etc/labweaver
COPY --from=builder /out/labweaver-gateway /usr/local/bin/labweaver-gateway
COPY access-gateway/sshd_config /etc/ssh/sshd_config
COPY access-gateway/target-ssh.conf /etc/labweaver/target-ssh.conf
COPY --chmod=0555 access-gateway/entrypoint.sh /usr/local/bin/labweaver-gateway-entrypoint
LABEL org.opencontainers.image.source="https://github.com/TeamMonad/LabWeaver" \
      org.opencontainers.image.revision="${SOURCE_COMMIT}" \
      org.opencontainers.image.title="openssh-gateway"
EXPOSE 2222
ENTRYPOINT ["/usr/local/bin/labweaver-gateway-entrypoint"]
