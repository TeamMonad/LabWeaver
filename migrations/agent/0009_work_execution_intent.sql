-- Durable Agent-owned intent and receipt for approved Work execution.
-- The JSON documents are private service state; the public execution request remains unchanged.

ALTER TABLE agent.agent_track_work_items
    ADD COLUMN execution_request jsonb,
    ADD COLUMN execution_receipt jsonb,
    ADD CONSTRAINT agent_track_work_items_execution_request_object_check
        CHECK (execution_request IS NULL OR jsonb_typeof(execution_request) = 'object'),
    ADD CONSTRAINT agent_track_work_items_execution_receipt_object_check
        CHECK (execution_receipt IS NULL OR jsonb_typeof(execution_receipt) = 'object');
