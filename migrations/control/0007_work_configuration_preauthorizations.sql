-- Control-owned concrete grants for reusing one exact immutable Work configuration plan.
-- The grant carries artifact identities and plan revision so a newly generated script cannot be
-- admitted by matching only an actor, model label, or restart flag.

CREATE TABLE control.work_configuration_preauthorizations (
    preauthorization_id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    environment_id uuid NOT NULL,
    environment_revision bigint NOT NULL CHECK (environment_revision > 0),
    actor_id uuid NOT NULL,
    plan_id uuid NOT NULL,
    plan_revision bigint NOT NULL CHECK (plan_revision > 0),
    script_artifact jsonb NOT NULL CHECK (jsonb_typeof(script_artifact) = 'object'),
    verification_script_artifact jsonb
        CHECK (verification_script_artifact IS NULL OR jsonb_typeof(verification_script_artifact) = 'object'),
    expires_at timestamptz NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, preauthorization_id, revision)
);

CREATE INDEX work_configuration_preauthorizations_project_environment_idx
    ON control.work_configuration_preauthorizations
        (project_id, environment_id, actor_id, expires_at, preauthorization_id);

CREATE INDEX work_configuration_preauthorizations_plan_idx
    ON control.work_configuration_preauthorizations
        (project_id, plan_id, plan_revision);
