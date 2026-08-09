-- Public release lifecycle and owner-scoped terminal result read paths.

CREATE TABLE evaluation_release_withdrawals (
    release_id uuid PRIMARY KEY REFERENCES evaluation_releases(release_id),
    course_id uuid NOT NULL,
    release_revision bigint NOT NULL CHECK (release_revision > 1),
    withdrawn_by uuid NOT NULL,
    reason_code text NOT NULL CHECK (length(reason_code) BETWEEN 1 AND 128),
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 128),
    request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    trace_id text NOT NULL CHECK (length(trace_id) BETWEEN 1 AND 128),
    withdrawn_at timestamptz NOT NULL
);

CREATE INDEX evaluation_release_withdrawals_course_time_idx
    ON evaluation_release_withdrawals (course_id, withdrawn_at DESC, release_id DESC);

CREATE INDEX evaluation_releases_course_published_idx
    ON evaluation_releases (course_id, published_at DESC, release_id DESC);

CREATE INDEX evaluation_runs_student_terminal_idx
    ON evaluation_runs (course_id, actor_id, updated_at DESC, run_id DESC)
    WHERE state IN ('succeeded', 'failed', 'cancelled');
