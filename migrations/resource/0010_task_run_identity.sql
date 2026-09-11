-- Each Evaluation attempt owns exactly one Resource request identity.
-- Environment targets keep a NULL task_run_id and are unaffected by this index.
CREATE UNIQUE INDEX resource_requests_task_run_id_unique
    ON resource.resource_requests (task_run_id)
    WHERE target_kind = 'task';
