ARG LABWEAVER_SERVICE_IMAGE
FROM ${LABWEAVER_SERVICE_IMAGE} AS labweaver-service
FROM nvidia/cuda:12.4.1-devel-ubuntu22.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        ca-certificates \
        python3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=labweaver-service /usr/local/bin/labweaver-service /usr/local/bin/labweaver-service

COPY tests /opt/labweaver/hidden-tests
COPY scripts /opt/labweaver/scripts
COPY profiles /opt/labweaver/profiles

RUN chmod -R a+rX /opt/labweaver

USER 65532:65532
WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/labweaver-service"]
