-- A build request owns its audit artifact, while the immutable image digest is
-- content-addressed and may legitimately be reproduced by another request.
ALTER TABLE image_artifacts
    DROP CONSTRAINT image_artifacts_image_digest_key;

CREATE INDEX image_artifacts_image_digest_idx
    ON image_artifacts (image_digest);
