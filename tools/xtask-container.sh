#!/bin/sh
# Run the Linux-only `cargo xtask` repository workflows inside the pinned
# container controller, so non-Linux developer hosts can drive the same
# fail-closed infrastructure entry points without a native Linux controller.
#
# The ansible-rs based playbook path runs unchanged inside the container;
# xtask still verifies the locked ansible-core and Python module versions
# before every playbook. All controller identity inputs remain explicit:
#
#   LABWEAVER_RUN_ID / LABWEAVER_TESTFLIGHT_RUN_ID / LABWEAVER_SOURCE_COMMIT
#   LABWEAVER_ANSIBLE_DEPENDENCY_ROOT   mounted read-only at /deps
#   LABWEAVER_CONTROLLER_IDENTITY_FILE  mounted read-only at the fixed
#                                       /run/labweaver/controller-identity/locator
#
# The host /etc/machine-id is mounted read-only when present so the locator
# machine binding still applies; a host without one fails inside xtask with
# the standard identity diagnostic. Extra bind mounts (for example the SSH
# key referenced by the vault inventory) can be supplied as comma-separated
# host=container pairs through LABWEAVER_XTASK_CONTAINER_MOUNTS.
#
# Usage: tools/xtask-container.sh <xtask arguments>
# Example: tools/xtask-container.sh platform-foundation --infra --env demo --yes
set -eu

# Git-Bash/MSYS rewrites POSIX-looking arguments (for example `-w /repo`) into
# Windows paths before docker sees them. This wrapper converts host paths
# itself, so keep the container-side arguments untouched.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

IMAGE="labweaver/xtask-controller:1.97.1-ansible-2.18.6"
CARGO_TARGET_VOLUME="labweaver-xtask-cargo-target"
CARGO_REGISTRY_VOLUME="labweaver-xtask-cargo-registry"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v docker >/dev/null 2>&1; then
    echo "[XTASK_CONTAINER] docker is required to run the containerized xtask controller" >&2
    exit 1
fi

# Render a host path the local Docker daemon accepts. MSYS/Git-Bash style
# /drive/... paths are rewritten to drive:/... form; everything else is kept.
to_host_path() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -m "$1"
        return
    fi
    printf '%s\n' "$1" | sed -E 's#^/([A-Za-z])(/.*)?$#\1:\2#'
}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "[XTASK_CONTAINER] building $IMAGE" >&2
    docker build \
        --file "$ROOT/containers/Containerfile.xtask-controller" \
        --tag "$IMAGE" \
        "$ROOT/containers"
fi

RUN_ARGS="--rm -w /repo"
if [ -t 0 ] && [ -t 1 ]; then
    RUN_ARGS="$RUN_ARGS -it"
else
    RUN_ARGS="$RUN_ARGS -i"
fi
RUN_ARGS="$RUN_ARGS -v $(to_host_path "$ROOT"):/repo"
RUN_ARGS="$RUN_ARGS -v $CARGO_TARGET_VOLUME:/repo/target"
RUN_ARGS="$RUN_ARGS -v $CARGO_REGISTRY_VOLUME:/usr/local/cargo/registry"

if [ -f /etc/machine-id ]; then
    RUN_ARGS="$RUN_ARGS -v /etc/machine-id:/etc/machine-id:ro"
fi

ENV_ARGS=""
APPEND_ENV() {
    if [ -z "$ENV_ARGS" ]; then
        ENV_ARGS="-e $1"
    else
        ENV_ARGS="$ENV_ARGS -e $1"
    fi
}

if [ -n "${LABWEAVER_ANSIBLE_DEPENDENCY_ROOT:-}" ]; then
    RUN_ARGS="$RUN_ARGS -v $(to_host_path "$LABWEAVER_ANSIBLE_DEPENDENCY_ROOT"):/deps:ro"
    APPEND_ENV "LABWEAVER_ANSIBLE_DEPENDENCY_ROOT=/deps"
fi

if [ -n "${LABWEAVER_CONTROLLER_IDENTITY_FILE:-}" ]; then
    RUN_ARGS="$RUN_ARGS -v $(to_host_path "$LABWEAVER_CONTROLLER_IDENTITY_FILE"):/run/labweaver/controller-identity/locator:ro"
    APPEND_ENV "LABWEAVER_CONTROLLER_IDENTITY_FILE=/run/labweaver/controller-identity/locator"
fi

# Forward every remaining LABWEAVER_* binding unchanged; xtask stays
# fail-closed when a required binding is absent.
while IFS= read -r line; do
    case "$line" in
        LABWEAVER_ANSIBLE_DEPENDENCY_ROOT=* | LABWEAVER_CONTROLLER_IDENTITY_FILE=*) ;;
        LABWEAVER_*) APPEND_ENV "$line" ;;
    esac
done <<EOF
$(env)
EOF

if [ -n "${LABWEAVER_XTASK_CONTAINER_MOUNTS:-}" ]; then
    remaining="$LABWEAVER_XTASK_CONTAINER_MOUNTS"
    while [ -n "$remaining" ]; do
        pair="${remaining%%,*}"
        case "$remaining" in
            *,*) remaining="${remaining#*,}" ;;
            *) remaining="" ;;
        esac
        host_part="${pair%%=*}"
        container_part="${pair#*=}"
        if [ -z "$host_part" ] || [ "$container_part" = "$pair" ] || [ -z "$container_part" ]; then
            echo "[XTASK_CONTAINER] LABWEAVER_XTASK_CONTAINER_MOUNTS entries must be host=container pairs: $pair" >&2
            exit 1
        fi
        RUN_ARGS="$RUN_ARGS -v $(to_host_path "$host_part"):$container_part"
    done
fi

# shellcheck disable=SC2086
exec docker run $RUN_ARGS "$IMAGE" cargo run --quiet -p xtask -- "$@"
