#!/bin/sh
set -eu

# Disposable NATS provider for the local Kind stack.  The listener is started
# by nats-box; nats reply invokes this same file once per request with the body
# in NATS_REQUEST_BODY.  The request envelope remains the real agent-provider
# contract, while the side effects are limited to copying one already-built
# OCI image inside the run-owned registry.

if [ -z "${NATS_REQUEST_BODY+x}" ]; then
    : "${NATS_SERVER:?NATS_SERVER is required}"
    : "${NATS_SUBJECT:?NATS_SUBJECT is required}"
    exec nats --server "$NATS_SERVER" \
        --creds /etc/labweaver/fixture/nats.creds \
        --tlsca /etc/labweaver/fixture/nats-ca.pem \
        --tlscert /etc/labweaver/fixture/nats-client.crt \
        --tlskey /etc/labweaver/fixture/nats-client.key \
        reply "$NATS_SUBJECT" \
        --command /etc/labweaver/fixture-script/build-executor-fixture.sh
fi

: "${NATS_REQUEST_BODY:?NATS_REQUEST_BODY is required}"

request_is_valid() {
    printf '%s\n' "$NATS_REQUEST_BODY" | jq -e '
        type == "object"
        and .protocolVersion == 2
        and (.buildRequestId | type == "string")
        and (.fenceGeneration | type == "number" and . > 0)
        and (.leaseToken | type == "string")
        and (.stageRequestId | type == "string")
        and (.deadlineAt | type == "string")
        and (.request | type == "object")
        and (
            (.request.kind == "ensure_private_project" and .stage == "ensure_private_project"
                and (.request.command.request.id == .buildRequestId)
                and (.request.identity | type == "string"))
            or
            (.request.kind == "build" and .stage == "build"
                and (.request.command.request.id == .buildRequestId)
                and (.request.identity | type == "string"))
            or
            (.request.kind == "publish" and .stage == "publish"
                and (.request.candidate.buildRequestId == .buildRequestId)
                and (.request.candidate.buildIdentity | type == "string")
                and (.request.candidate.digest | type == "string"))
            or
            (.request.kind == "cleanup" and .stage == "cleanup"
                and (.request.buildRequestId == .buildRequestId)
                and (.request.identity | type == "string"))
        )
    ' >/dev/null
}

if ! request_is_valid; then
    # A malformed request must fail closed.  Returning no reply makes the
    # agent transport report an unavailable provider instead of inventing a
    # successful build result.
    exit 1
fi

request_kind=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.request.kind')
build_request_id=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.buildRequestId')
stage_request_id=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.stageRequestId')
protocol_version=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.protocolVersion')
fence_generation=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.fenceGeneration')
stage=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.stage')

response_json=''

copy_source_image() {
    : "${FIXTURE_SOURCE_IMAGE:?FIXTURE_SOURCE_IMAGE is required}"
    : "${FIXTURE_REGISTRY_BASE:?FIXTURE_REGISTRY_BASE is required}"
    source_repository=${FIXTURE_SOURCE_IMAGE%@*}
    source_digest=${FIXTURE_SOURCE_IMAGE##*@}
    target_repository=$1
    case "$source_repository:$source_digest:$target_repository" in
        *[[:space:]]*) exit 1 ;;
    esac
    case "$source_repository" in
        *@*|''|*'/'*) ;;
        *) exit 1 ;;
    esac
    case "$source_digest" in
        sha256:[0-9a-fA-F][0-9a-fA-F]*) ;;
        *) exit 1 ;;
    esac
    case "$target_repository" in
        ''|*'@'*|*'/'*'/'*'..'*|*[[:space:]]*) exit 1 ;;
    esac

    source_path=${source_repository#*/}
    target_path=${target_repository#*/}
    [ "$source_path" != "$source_repository" ]
    [ "$target_path" != "$target_repository" ]
    base=${FIXTURE_REGISTRY_BASE%/}
    temp_dir=$(mktemp -d)
    trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
    manifest_file=$temp_dir/manifest.json
    headers_file=$temp_dir/headers
    curl -fsS -D "$headers_file" \
        -H 'Accept: application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json' \
        "$base/v2/$source_path/manifests/$source_digest" \
        -o "$manifest_file"
    media_type=$(jq -er '.mediaType // "application/vnd.oci.image.manifest.v1+json"' "$manifest_file")
    blob_digests=$(jq -er '[.config.digest, (.layers[]?.digest)] | map(select(type == "string")) | unique | .[]' "$manifest_file")
    [ -n "$blob_digests" ]
    for blob in $blob_digests; do
        mount_status=$(curl -sS -o /dev/null -w '%{http_code}' -X POST \
            "$base/v2/$target_path/blobs/uploads/?mount=$blob&from=$source_path")
        if [ "$mount_status" = 201 ]; then
            continue
        fi
        upload_headers=$temp_dir/upload-headers
        curl -fsS -D "$upload_headers" -o /dev/null -X POST \
            "$base/v2/$target_path/blobs/uploads/"
        location=$(awk 'tolower($1) == "location:" { sub("\r$", "", $2); print $2; exit }' "$upload_headers")
        [ -n "$location" ]
        case "$location" in
            http://*|https://*) ;;
            /*) location=$base$location ;;
            *) location=$base/$location ;;
        esac
        blob_file=$temp_dir/${blob#sha256:}
        curl -fsS "$base/v2/$source_path/blobs/$blob" -o "$blob_file"
        case "$location" in
            *'?'*) separator='&' ;;
            *) separator='?' ;;
        esac
        curl -fsS -X PUT -H 'Content-Type: application/octet-stream' \
            --data-binary "@$blob_file" "$location${separator}digest=$blob" \
            >/dev/null
    done
    curl -fsS -X PUT -H "Content-Type: $media_type" \
        --data-binary "@$manifest_file" "$base/v2/$target_path/manifests/$source_digest" \
        >/dev/null
    trap - EXIT HUP INT TERM
    rm -rf "$temp_dir"
    printf '%s' "$source_digest"
}

case "$request_kind" in
    ensure_private_project)
        output_repository=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.request.command.request.outputRepository')
        repository_prefix=${output_repository%/*}
        [ "$repository_prefix" != "$output_repository" ]
        response_json=$(jq -cn \
            --arg id "$build_request_id" \
            --arg identity "$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.request.identity')" \
            --arg prefix "$repository_prefix" \
            '{status:"private_project_ready", project:{buildRequestId:$id,buildIdentity:$identity,repositoryPrefix:$prefix,private:true,storageQuotaBytes:10737418240,robotSubject:"runtime-puller"}}')
        ;;
    build)
        output_repository=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.request.command.request.outputRepository')
        digest=$(copy_source_image "$output_repository")
        response_json=$(jq -cn \
            --arg id "$build_request_id" \
            --arg identity "$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.request.identity')" \
            --arg repository "$output_repository" \
            --arg digest "$digest" \
            '{status:"built",candidate:{buildRequestId:$id,buildIdentity:$identity,repository:$repository,digest:$digest}}')
        ;;
    publish)
        response_json=$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -c \
            '{status:"published",image:{buildIdentity:.request.candidate.buildIdentity,digest:.request.candidate.digest}}')
        ;;
    cleanup)
        response_json=$(jq -cn \
            --arg id "$build_request_id" \
            --arg identity "$(printf '%s\n' "$NATS_REQUEST_BODY" | jq -er '.request.identity')" \
            '{status:"cleaned",buildRequestId:$id,buildIdentity:$identity}')
        ;;
    *)
        exit 1
        ;;
esac

printf '%s\n' "$NATS_REQUEST_BODY" | jq -c \
    --argjson response "$response_json" \
    --arg protocolVersion "$protocol_version" \
    --arg buildRequestId "$build_request_id" \
    --argjson fenceGeneration "$fence_generation" \
    --arg stage "$stage" \
    --arg stageRequestId "$stage_request_id" \
    '{protocolVersion:($protocolVersion|tonumber),buildRequestId:$buildRequestId,
      fenceGeneration:$fenceGeneration,stage:$stage,stageRequestId:$stageRequestId,
      response:$response}'
