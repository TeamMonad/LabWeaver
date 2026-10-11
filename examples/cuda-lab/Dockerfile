ARG CUDA_BASE_IMAGE=nvidia/cuda:12.4.1-devel-ubuntu22.04
FROM ${CUDA_BASE_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        ca-certificates \
        python3 \
    && rm -rf /var/lib/apt/lists/*

COPY student /opt/labweaver/workspace-seed/student
COPY README.md /opt/labweaver/workspace-seed/README.md
COPY statement.md /opt/labweaver/workspace-seed/statement.md
COPY scripts /opt/labweaver/scripts

RUN chmod -R a+rX /opt/labweaver/workspace-seed \
    && chmod 0755 /opt/labweaver/scripts/local-test.sh \
    && mkdir -p /workspace /tmp \
    && chmod 0777 /workspace /tmp

USER 65534:65534
WORKDIR /workspace
EXPOSE 8080
CMD ["/usr/bin/python3", "-m", "http.server", "8080", "--bind", "0.0.0.0", "--directory", "/workspace"]
