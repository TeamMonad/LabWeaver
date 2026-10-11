ARG LABWEAVER_SERVICE_IMAGE
FROM ${LABWEAVER_SERVICE_IMAGE} AS labweaver-service
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        binutils-riscv64-unknown-elf \
        ca-certificates \
        coreutils \
        gcc \
        gcc-riscv64-unknown-elf \
        gzip \
        libc6-dev \
        make \
        perl \
        python3 \
        qemu-system-misc \
        tar \
    && rm -rf /var/lib/apt/lists/*

COPY --from=labweaver-service /usr/local/bin/labweaver-service /usr/local/bin/labweaver-service

COPY xv6 /opt/labweaver/xv6
COPY scripts /opt/labweaver/scripts
COPY profiles /opt/labweaver/profiles
COPY tests /opt/labweaver/hidden-tests

RUN chmod 0755 /opt/labweaver/scripts/*.sh /opt/labweaver/xv6/test-xv6.py \
    && chmod -R a+rX /opt/labweaver

USER 65532:65532
WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/labweaver-service"]
