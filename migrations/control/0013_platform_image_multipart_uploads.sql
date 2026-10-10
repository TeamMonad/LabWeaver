-- Platform image archives use one durable S3 multipart upload.  The old direct PUT
-- sessions are terminalized before the new fields are introduced; their exact object
-- keys remain eligible for the existing version cleanup reconciliation after expiry.

UPDATE control.platform_image_upload_sessions
   SET state = 'failed',
       terminal_diagnostic = 'LW_PLATFORM_IMAGE_UPLOAD_PROTOCOL_RETIRED',
       cancel_requested = false,
       completion_lease_token = NULL,
       completion_lease_expires_at = NULL,
       revision = revision + 1,
       cleanup_versions_next_attempt_at = now(),
       updated_at = now()
 WHERE state IN ('pending', 'queued', 'freezing', 'importing', 'cancelling');

ALTER TABLE control.platform_image_upload_sessions
    ADD COLUMN multipart_upload_id text,
    ADD COLUMN multipart_part_size_bytes bigint,
    ADD COLUMN multipart_part_count integer,
    ADD COLUMN create_idempotency_key text,
    ADD COLUMN multipart_creation_lease_token uuid,
    ADD COLUMN multipart_creation_lease_expires_at timestamptz,
    ADD COLUMN multipart_complete_started boolean NOT NULL DEFAULT false;

ALTER TABLE control.platform_image_upload_sessions
    ADD CONSTRAINT platform_image_upload_sessions_multipart_identity_check
        CHECK (
            (multipart_upload_id IS NULL) =
            (multipart_part_size_bytes IS NULL AND multipart_part_count IS NULL)
        ),
    ADD CONSTRAINT platform_image_upload_sessions_multipart_part_shape_check
        CHECK (
            multipart_part_size_bytes IS NULL
            OR (multipart_part_size_bytes = 67108864 AND multipart_part_count > 0)
        ),
    ADD CONSTRAINT platform_image_upload_sessions_multipart_creation_lease_check
        CHECK (
            (multipart_creation_lease_token IS NULL) =
            (multipart_creation_lease_expires_at IS NULL)
        ),
    ADD CONSTRAINT platform_image_upload_sessions_multipart_required_for_active_check
        CHECK (
            state IN ('pending', 'imported', 'failed', 'cancelled')
            OR (
                multipart_upload_id IS NOT NULL
                AND multipart_part_size_bytes IS NOT NULL
                AND multipart_part_count IS NOT NULL
            )
        );

CREATE TABLE control.platform_image_upload_parts (
    upload_id uuid NOT NULL REFERENCES control.platform_image_upload_sessions(upload_id) ON DELETE CASCADE,
    part_number integer NOT NULL CHECK (part_number > 0),
    expected_size_bytes bigint NOT NULL CHECK (expected_size_bytes > 0),
    upload_url text NOT NULL CHECK (upload_url <> ''),
    required_headers jsonb NOT NULL,
    expires_at timestamptz NOT NULL,
    requested_etag text,
    etag text,
    observed_size_bytes bigint,
    PRIMARY KEY (upload_id, part_number),
    CHECK ((etag IS NULL) = (observed_size_bytes IS NULL)),
    CHECK (observed_size_bytes IS NULL OR observed_size_bytes > 0)
);

CREATE INDEX platform_image_upload_parts_pending_idx
    ON control.platform_image_upload_parts (upload_id, part_number)
    WHERE etag IS NULL;
