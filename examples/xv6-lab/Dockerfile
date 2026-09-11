FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        binutils-riscv64-unknown-elf \
        coreutils \
        gcc \
        gcc-riscv64-unknown-elf \
        libc6-dev \
        make \
        perl \
        python3 \
        qemu-system-misc \
    && rm -rf /var/lib/apt/lists/*

COPY xv6 /opt/labweaver/xv6
COPY scripts /opt/labweaver/scripts
COPY student /opt/labweaver/student
COPY tests /opt/labweaver/tests
COPY README.md /opt/labweaver/workspace-seed/README.md
COPY profiles /opt/labweaver/workspace-seed/profiles
COPY student /opt/labweaver/workspace-seed/student
COPY tests /opt/labweaver/workspace-seed/tests

RUN mkdir -p /work /workspace /tmp \
    && chmod 0755 /opt/labweaver/scripts/*.sh /opt/labweaver/xv6/test-xv6.py \
    && chmod -R a+rX /opt/labweaver/workspace-seed \
    && chmod 0777 /work /workspace /tmp

USER 65534:65534
WORKDIR /workspace
CMD ["/usr/bin/python3", "-m", "http.server", "8080", "--bind", "0.0.0.0", "--directory", "/workspace"]
