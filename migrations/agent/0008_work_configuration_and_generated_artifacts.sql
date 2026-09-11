-- Persist the Work configuration waiting state and Agent-owned generated object
-- resolutions used by Control and Environment after a plan is materialized.

ALTER TABLE agent.agent_track_work_items
    DROP CONSTRAINT IF EXISTS agent_track_work_items_state_check,
    ADD CONSTRAINT agent_track_work_items_state_check
        CHECK (state IN ('requested', 'running', 'succeeded', 'awaiting_approval', 'failed', 'cancelled'));

CREATE TABLE agent.generated_artifacts (
    artifact_id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    course_id uuid,
    package_id uuid NOT NULL,
    package_revision bigint NOT NULL CHECK (package_revision > 0),
    artifact_kind text NOT NULL CHECK (artifact_kind IN ('build_context', 'work_script', 'verification_script')),
    object_key text NOT NULL UNIQUE CHECK (
        object_key <> ''
        AND length(object_key) <= 2048
        AND position('..' IN object_key) = 0
        AND object_key !~ '[[:cntrl:]]'
    ),
    object_version text NOT NULL CHECK (object_version <> ''),
    size_bytes bigint NOT NULL CHECK (size_bytes > 0),
    content_sha256 text NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    store_binding text NOT NULL CHECK (store_binding <> ''),
    media_type text NOT NULL CHECK (media_type <> ''),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX generated_artifacts_scope_idx
    ON agent.generated_artifacts (project_id, course_id, package_id, package_revision, artifact_kind);
