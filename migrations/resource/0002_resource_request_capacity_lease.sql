-- Resource request approval, exact capacity reservation, and Lease lifecycle.
-- All cross-domain effects are emitted through resource.outbox_events in the same transaction.

CREATE TABLE resource_requests (
    request_id uuid PRIMARY KEY,
    generation bigint NOT NULL CHECK (generation > 0),
    request_key text NOT NULL CHECK (request_key ~ '^[a-z0-9][a-z0-9_-]{0,95}$'),
    requester_id uuid NOT NULL,
    course_id uuid NOT NULL,
    project_id uuid,
    environment_id uuid NOT NULL UNIQUE,
    release_id uuid NOT NULL,
    release_version bigint NOT NULL CHECK (release_version > 0),
    release_sha256 text NOT NULL CHECK (release_sha256 ~ '^[0-9a-f]{64}$'),
    requested_cpu_millicores integer NOT NULL CHECK (requested_cpu_millicores > 0),
    requested_memory_bytes bigint NOT NULL CHECK (requested_memory_bytes > 0),
    requested_storage_bytes bigint NOT NULL CHECK (requested_storage_bytes > 0),
    gpu_class text,
    gpu_count integer,
    requested_duration_seconds bigint NOT NULL CHECK (requested_duration_seconds > 0),
    state text NOT NULL CHECK (state IN ('reviewing','allocating','active','expiring','expired','rejected','cancelled')),
    revision bigint NOT NULL CHECK (revision > 0),
    diagnostic_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((gpu_class IS NULL) = (gpu_count IS NULL)),
    CHECK (gpu_count IS NULL OR gpu_count > 0)
);

CREATE UNIQUE INDEX resource_requests_live_without_project_key
    ON resource_requests (requester_id, course_id, request_key)
    WHERE project_id IS NULL AND state IN ('reviewing','allocating','active','expiring');
CREATE UNIQUE INDEX resource_requests_live_with_project_key
    ON resource_requests (requester_id, course_id, project_id, request_key)
    WHERE project_id IS NOT NULL AND state IN ('reviewing','allocating','active','expiring');

CREATE TABLE resource_request_transitions (
    request_id uuid NOT NULL REFERENCES resource_requests(request_id),
    sequence bigint NOT NULL CHECK (sequence > 0),
    from_state text,
    to_state text NOT NULL,
    actor_id uuid,
    diagnostic_code text,
    trace_id text NOT NULL CHECK (length(trace_id) BETWEEN 1 AND 128),
    occurred_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (request_id, sequence)
);

CREATE TABLE resource_approvals (
    approval_id uuid PRIMARY KEY,
    request_id uuid NOT NULL REFERENCES resource_requests(request_id),
    request_revision bigint NOT NULL CHECK (request_revision > 0),
    approver_id uuid NOT NULL,
    provider_binding text NOT NULL CHECK (length(provider_binding) BETWEEN 1 AND 120),
    policy_sha256 text NOT NULL CHECK (policy_sha256 ~ '^[0-9a-f]{64}$'),
    approved_cpu_millicores integer NOT NULL CHECK (approved_cpu_millicores > 0),
    approved_memory_bytes bigint NOT NULL CHECK (approved_memory_bytes > 0),
    approved_storage_bytes bigint NOT NULL CHECK (approved_storage_bytes > 0),
    approved_gpu_class text,
    approved_gpu_count integer,
    approved_duration_seconds bigint NOT NULL CHECK (approved_duration_seconds > 0),
    reason text NOT NULL CHECK (length(trim(reason)) BETWEEN 1 AND 500),
    valid_until timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    CHECK (valid_until > created_at),
    CHECK ((approved_gpu_class IS NULL) = (approved_gpu_count IS NULL)),
    CHECK (approved_gpu_count IS NULL OR approved_gpu_count > 0)
);

CREATE TABLE capacity_claims (
    claim_id uuid PRIMARY KEY,
    request_id uuid NOT NULL UNIQUE REFERENCES resource_requests(request_id),
    approval_id uuid NOT NULL UNIQUE REFERENCES resource_approvals(approval_id),
    provider_binding text NOT NULL,
    policy_sha256 text NOT NULL CHECK (policy_sha256 ~ '^[0-9a-f]{64}$'),
    quota_plan_sha256 text NOT NULL CHECK (quota_plan_sha256 ~ '^[0-9a-f]{64}$'),
    state text NOT NULL CHECK (state IN ('reserved','provisioning','ready','handed_off','releasing','released','blocked')),
    namespace_name text,
    namespace_uid text,
    quota_uid text,
    revision bigint NOT NULL CHECK (revision > 0),
    last_diagnostic_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE resource_leases (
    lease_id uuid PRIMARY KEY,
    request_id uuid NOT NULL UNIQUE REFERENCES resource_requests(request_id),
    claim_id uuid NOT NULL UNIQUE REFERENCES capacity_claims(claim_id),
    state text NOT NULL CHECK (state IN ('allocating','active','expiring','expired','revoked')),
    revision bigint NOT NULL CHECK (revision > 0),
    active_from timestamptz,
    expires_at timestamptz,
    revoke_reason_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((state = 'allocating') OR (active_from IS NOT NULL AND expires_at IS NOT NULL AND expires_at > active_from))
);

-- Lock rows serialize calculation of actor/course/project/provider aggregate usage.
CREATE TABLE quota_scope_locks (
    scope_kind text NOT NULL CHECK (scope_kind IN ('actor','course','project','provider')),
    scope_id text NOT NULL CHECK (length(scope_id) BETWEEN 1 AND 128),
    provider_binding text NOT NULL CHECK (length(provider_binding) BETWEEN 1 AND 120),
    PRIMARY KEY (scope_kind, scope_id, provider_binding)
);

CREATE TABLE capacity_attempts (
    claim_id uuid NOT NULL REFERENCES capacity_claims(claim_id),
    attempt bigint NOT NULL CHECK (attempt > 0),
    step text NOT NULL CHECK (step IN ('provision_namespace','provision_quota','handoff_environment','expire_environment','release_capacity')),
    state text NOT NULL CHECK (state IN ('pending','leased','retry','completed','failed')),
    lease_owner text,
    lease_token uuid,
    lease_expires_at timestamptz,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    diagnostic_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (claim_id, attempt, step),
    CHECK ((state = 'leased') = (lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL))
);
