-- Freeze authoring metering intent before deleting its runtime objects. Delivery retries
-- reuse the exact event identities and payload after a process restart.
ALTER TABLE agent.authoring_sandbox_attempts
    ADD COLUMN usage_payload jsonb,
    ADD COLUMN usage_delivered boolean NOT NULL DEFAULT false,
    ADD COLUMN usage_checkpointed_at timestamptz,
    ADD COLUMN usage_observation jsonb,
    ADD COLUMN usage_observed_at timestamptz,
    ADD COLUMN usage_diagnostic_code text
        CHECK (usage_diagnostic_code IS NULL OR length(usage_diagnostic_code) BETWEEN 1 AND 128),
    ADD CONSTRAINT authoring_sandbox_usage_payload_check
        CHECK (usage_payload IS NULL OR
               (jsonb_typeof(usage_payload) = 'array' AND jsonb_array_length(usage_payload) BETWEEN 1 AND 2)),
    ADD CONSTRAINT authoring_sandbox_usage_checkpoint_check
        CHECK ((usage_payload IS NULL) = (usage_checkpointed_at IS NULL)),
    ADD CONSTRAINT authoring_sandbox_usage_observation_check
        CHECK ((usage_observation IS NULL) = (usage_observed_at IS NULL)
               AND (usage_observation IS NULL OR jsonb_typeof(usage_observation) = 'object')),
    ADD CONSTRAINT authoring_sandbox_usage_delivery_check
        CHECK (NOT usage_delivered OR usage_payload IS NOT NULL);

CREATE INDEX authoring_sandbox_cleanup_due_idx
    ON agent.authoring_sandbox_attempts (updated_at, run_id, track, attempt_number)
    WHERE state IN ('terminal', 'failed', 'cleanup_confirmed')
       OR (state = 'released' AND (usage_payload IS NOT NULL OR usage_observation IS NOT NULL)
           AND NOT usage_delivered);
