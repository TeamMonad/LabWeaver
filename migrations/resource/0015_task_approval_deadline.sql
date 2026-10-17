-- Resource owns the original approval wait of each one-shot task.
ALTER TABLE resource_requests
    ADD COLUMN approval_deadline_at timestamptz,
    ADD CONSTRAINT task_approval_deadline_target
        CHECK (approval_deadline_at IS NULL OR
               (target_kind = 'task' AND approval_deadline_at >= created_at));

CREATE INDEX resource_task_approval_deadline
    ON resource_requests (approval_deadline_at, request_id)
    WHERE target_kind = 'task' AND state = 'reviewing';

-- Old rows have no recorded wait budget. The task owner must reconcile them
-- through the authenticated create command; this migration does not invent one.
