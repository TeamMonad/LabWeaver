-- Durable Agent-owned advisory LLM review queue and receipt.
-- Request and receipt documents stay private to Agent; the task identity is the
-- idempotent boundary shared with Evaluation.
CREATE TABLE agent.llm_review_runs (
    task_run_id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    course_id uuid,
    request_json jsonb NOT NULL CHECK (jsonb_typeof(request_json) = 'object'),
    request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('queued', 'running', 'cancelling', 'succeeded', 'failed', 'cancelled')),
    attempt bigint NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    lease_token uuid,
    worker_id text,
    heartbeat_at timestamptz,
    lease_expires_at timestamptz,
    receipt_json jsonb CHECK (receipt_json IS NULL OR jsonb_typeof(receipt_json) = 'object'),
    diagnostic_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);

CREATE INDEX llm_review_runs_due_idx
    ON agent.llm_review_runs (created_at, task_run_id)
    WHERE state IN ('queued', 'running', 'cancelling');

CREATE INDEX llm_review_runs_project_idx
    ON agent.llm_review_runs (project_id, course_id, created_at DESC, task_run_id);
