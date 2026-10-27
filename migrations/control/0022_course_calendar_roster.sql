-- Draft for Issue #219. Do not register until the product rule and shared
-- contract writer window are available.
-- Course calendar facts are Control-owned and all instants are timestamptz.
CREATE TABLE control.course_calendars (
    course_id uuid PRIMARY KEY,
    revision bigint NOT NULL CHECK (revision > 0),
    open_at timestamptz,
    due_at timestamptz NOT NULL,
    time_zone text NOT NULL CHECK (length(time_zone) BETWEEN 1 AND 128),
    late_policy text NOT NULL CHECK (late_policy IN ('hard_close', 'allow_with_mark')),
    late_until timestamptz,
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    created_at timestamptz NOT NULL DEFAULT date_trunc('milliseconds', clock_timestamp()),
    updated_at timestamptz NOT NULL DEFAULT date_trunc('milliseconds', clock_timestamp()),
    CHECK (open_at IS NULL OR open_at < due_at),
    CHECK ((late_policy = 'hard_close' AND late_until IS NULL)
        OR (late_policy = 'allow_with_mark' AND (late_until IS NULL OR late_until > due_at)))
);
-- The import itself uses the existing idempotency ledger. This audit table is
-- intentionally separate so a retried request can return per-row diagnostics
-- without creating a second membership or invitation.
CREATE TABLE control.course_roster_imports (
    import_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    requested_by uuid NOT NULL,
    request_sha256 text NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    revision bigint NOT NULL CHECK (revision > 0),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    created_at timestamptz NOT NULL DEFAULT date_trunc('milliseconds', clock_timestamp()),
    UNIQUE (course_id, request_sha256)
);
CREATE INDEX course_roster_imports_course_idx
    ON control.course_roster_imports(course_id, created_at DESC);
