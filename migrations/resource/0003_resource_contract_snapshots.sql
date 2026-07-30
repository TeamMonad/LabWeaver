-- Keep the strict public contract beside indexed authority columns. The JSON snapshot is the
-- canonical reconstruction input; indexed columns remain query and lock keys only.

ALTER TABLE resource_requests
    ADD COLUMN contract jsonb NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(contract) = 'object');
ALTER TABLE resource_approvals
    ADD COLUMN contract jsonb NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(contract) = 'object');
ALTER TABLE capacity_claims
    ADD COLUMN contract jsonb NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(contract) = 'object');
ALTER TABLE resource_leases
    ADD COLUMN contract jsonb NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(contract) = 'object');

-- A released deployment will only receive the follow-up migration after the Resource runtime
-- has been rolled out with compatible readers. Empty snapshots are intentionally rejected by
-- the runtime, never interpreted as a valid aggregate.
