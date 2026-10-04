FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        ca-certificates \
        python3 \
    && rm -rf /var/lib/apt/lists/*

COPY app /opt/ctf/app
COPY seed /opt/ctf/seed
COPY scripts /opt/labweaver/scripts
COPY README.md /opt/labweaver/workspace-seed/README.md
COPY statement.md /opt/labweaver/workspace-seed/statement.md

RUN python3 /opt/ctf/seed/seed.py \
    && chmod -R a+rX /opt/ctf /opt/labweaver/workspace-seed \
    && chmod 0755 /opt/labweaver/scripts/local-test.sh \
    && mkdir -p /workspace /tmp \
    && chmod 0777 /workspace /tmp

USER 65534:65534
WORKDIR /workspace
EXPOSE 8080
CMD ["/usr/bin/python3", "/opt/ctf/app/server.py"]
