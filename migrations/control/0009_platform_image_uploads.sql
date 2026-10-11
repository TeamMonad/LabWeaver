-- Control-owned staging record for one administrator OCI archive upload.
-- The archive itself is frozen in the immutable object store; this row is the
-- completion fence and the cleanup ledger source, and it never becomes a second
-- authority for the Agent-owned platform image catalog.

CREATE TABLE control.platform_image_upload_sessions (
    upload_id uuid PRIMARY KEY,
    created_by uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('container', 'virtual_machine')),
    binding text NOT NULL CHECK (binding ~ '^[a-z0-9][a-z0-9._-]{0,127}$'),
    target_reference text NOT NULL CHECK (target_reference <> '' AND target_reference !~ '\s'),
    trust_revision bigint NOT NULL CHECK (trust_revision > 0),
    reason text NOT NULL CHECK (reason <> '' AND length(reason) <= 512),
    archive_bytes bigint NOT NULL CHECK (archive_bytes > 0),
    archive_media_type text NOT NULL CHECK (archive_media_type <> ''),
    object_key text NOT NULL UNIQUE,
    artifact_id uuid UNIQUE,
    object_version text,
    state text NOT NULL CHECK (state IN ('pending', 'importing', 'imported', 'failed')),
    terminal_diagnostic text,
    imported_catalog_id uuid,
    completion_idempotency_key text,
    completion_request_sha256 text CHECK (completion_request_sha256 ~ '^[0-9a-f]{64}$'),
    completion_lease_token uuid,
    completion_lease_expires_at timestamptz,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((object_version IS NULL) = (artifact_id IS NULL)),
    CHECK ((completion_idempotency_key IS NULL) = (completion_request_sha256 IS NULL)),
    CHECK ((completion_lease_token IS NULL) = (completion_lease_expires_at IS NULL)),
    CHECK ((state = 'importing') = (completion_lease_token IS NOT NULL)),
    CHECK ((state = 'imported') = (imported_catalog_id IS NOT NULL))
);

CREATE INDEX platform_image_upload_sessions_due_idx
    ON control.platform_image_upload_sessions (state, completion_lease_expires_at, expires_at);
