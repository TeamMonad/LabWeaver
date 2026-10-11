-- Each schema invocation is one separately admitted execution generation.
ALTER TABLE agent.authoring_sandbox_attempts
    DROP CONSTRAINT authoring_sandbox_attempts_pkey,
    ADD PRIMARY KEY (run_id, track, attempt_number, execution_generation),
    ALTER COLUMN binding DROP NOT NULL,
    ADD COLUMN stderr_object_key text,
    ADD COLUMN export_object_key text,
    ADD COLUMN terminal_receipt jsonb,
    ADD COLUMN request_payload jsonb CHECK (request_payload IS NULL OR (jsonb_typeof(request_payload)='object' AND octet_length(request_payload::text)<=4096)),
    ADD CONSTRAINT authoring_sandbox_admitted_binding CHECK
        (state IN ('creating','failed','cleanup_confirmed','released') OR binding IS NOT NULL),
    ADD CONSTRAINT authoring_sandbox_receipt_bounded CHECK
        (terminal_receipt IS NULL OR
         (jsonb_typeof(terminal_receipt) = 'object' AND octet_length(terminal_receipt::text) <= 16384));

-- Assigned only when a new track attempt is claimed, never by heartbeat or recovery.
-- Historical attempts have no known start and must not gain invented recovery metadata.
ALTER TABLE agent.agent_track_work_items ADD COLUMN attempt_started_at timestamptz;
