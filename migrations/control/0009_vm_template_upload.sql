-- Reviewed virtual-machine disk descriptor for one Control-owned platform image staging session.
--
-- A container upload always imports an OCI layout and carries no descriptor. A raw/qcow2
-- virtual-machine upload freezes one disk inside the staged archive and must carry the format,
-- the relative in-archive path and the reviewed capacity so the Agent can wrap exactly that disk.
-- The three columns are nullable as a triple: a partially declared descriptor can never be
-- persisted, so a misdeclared VM upload fails closed instead of being imported as a containerdisk.
ALTER TABLE control.platform_image_upload_sessions
    ADD COLUMN disk_format text
        CHECK (disk_format IS NULL OR disk_format IN ('qcow2', 'raw')),
    ADD COLUMN disk_path text
        CHECK (
            disk_path IS NULL
            OR (disk_path <> '' AND length(disk_path) <= 256 AND disk_path !~ '^/|/$|\.\.')
        ),
    ADD COLUMN capacity_bytes bigint
        CHECK (capacity_bytes IS NULL OR capacity_bytes > 0);

ALTER TABLE control.platform_image_upload_sessions
    ADD CONSTRAINT platform_image_upload_sessions_disk_descriptor_together_check
    CHECK ((disk_format IS NULL) = (disk_path IS NULL) AND (disk_path IS NULL) = (capacity_bytes IS NULL));
