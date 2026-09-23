-- Virtual-machine base-disk descriptor on the platform image catalog.
--
-- A kind='virtual_machine' row is the published containerdisk identity: the reviewed reference
-- and its resolved manifest digest, plus the reviewed capacity of the wrapped disk, the SHA-256
-- of the raw disk bytes inside the archive, and the declared encoding. The descriptor is a
-- consistent triple: a registry-reference inventory row carries none of the three, an imported
-- raw/qcow2 upload carries all three with a positive capacity. Container rows never carry it, so
-- a misdeclared upload can never be persisted as a bare container pin.
ALTER TABLE agent.platform_image_catalog
    ADD COLUMN capacity_bytes bigint,
    ADD COLUMN disk_sha256 text,
    ADD COLUMN format text,
    ADD CONSTRAINT platform_image_catalog_disk_sha256_shape
        CHECK (disk_sha256 IS NULL OR disk_sha256 ~ '^[0-9a-f]{64}$'),
    ADD CONSTRAINT platform_image_catalog_disk_format_shape
        CHECK (format IS NULL OR format IN ('qcow2', 'raw')),
    ADD CONSTRAINT platform_image_catalog_disk_descriptor_shape
        CHECK (
            (capacity_bytes IS NULL AND disk_sha256 IS NULL AND format IS NULL)
            OR (capacity_bytes > 0 AND disk_sha256 IS NOT NULL AND format IS NOT NULL)
        ),
    ADD CONSTRAINT platform_image_catalog_container_disk_descriptor_absent
        CHECK (
            kind <> 'container'
            OR (capacity_bytes IS NULL AND disk_sha256 IS NULL AND format IS NULL)
        );
