-- Resource owns the original approval wait of each one-shot task.
ALTER TABLE resource_requests
    ADD COLUMN approval_deadline_at timestamptz,
    ADD CONSTRAINT task_approval_deadline_target
        CHECK (approval_deadline_at IS NULL OR
               (target_kind = 'task' AND approval_deadline_at >= created_at));

CREATE INDEX resource_task_approval_deadline
    ON resource_requests (approval_deadline_at, request_id)
    WHERE target_kind = 'task' AND state = 'reviewing';

-- Old rows have no recorded wait budget. Resource refuses readiness while any old Reviewing
-- task remains, so operators must reconcile them through an authenticated workflow.
