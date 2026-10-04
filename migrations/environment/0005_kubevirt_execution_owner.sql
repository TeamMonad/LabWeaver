-- The deployment coordinator must confirm all old executor incarnations terminated
-- and conditionally finalize each ownerless in-flight request before this hard cut.
-- Deadline expiry alone is never evidence that its backend future stopped.
ALTER TABLE kubevirt_executor_fences ADD COLUMN execution_owner jsonb;
ALTER TABLE kubevirt_executor_fences
    ADD CONSTRAINT kubevirt_execution_owner_shape
        CHECK (execution_owner IS NULL OR jsonb_typeof(execution_owner) = 'object'),
    ADD CONSTRAINT kubevirt_inflight_execution_owner_required
        CHECK (last_response IS NOT NULL OR execution_owner IS NOT NULL);
