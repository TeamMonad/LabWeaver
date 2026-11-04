-- Course experiment policy and immutable group payer binding.
CREATE TABLE control.course_experiments (
    experiment_id uuid PRIMARY KEY,
    course_id uuid NOT NULL,
    release_id uuid NOT NULL,
    policy text NOT NULL CHECK (policy IN ('individual','group')),
    revision bigint NOT NULL CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT date_trunc('milliseconds', clock_timestamp()),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    UNIQUE (course_id, experiment_id)
);
CREATE INDEX course_experiments_course_idx ON control.course_experiments(course_id, experiment_id);
CREATE TABLE control.experiment_groups (
    group_id uuid PRIMARY KEY,
    experiment_id uuid NOT NULL REFERENCES control.course_experiments(experiment_id),
    payer_project_id uuid NOT NULL,
    payer_actor_id uuid NOT NULL,
    payer_consent boolean NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT date_trunc('milliseconds', clock_timestamp()),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    UNIQUE (experiment_id, group_id)
);
CREATE TABLE control.experiment_group_members (
    group_id uuid NOT NULL REFERENCES control.experiment_groups(group_id),
    actor_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    state text NOT NULL CHECK (state IN ('active','revoked')),
    PRIMARY KEY (group_id, actor_id)
);
CREATE INDEX experiment_group_members_actor_idx ON control.experiment_group_members(actor_id, group_id);
CREATE OR REPLACE FUNCTION control.prevent_experiment_payer_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.payer_project_id <> OLD.payer_project_id
       OR NEW.payer_actor_id <> OLD.payer_actor_id
       OR NEW.payer_consent <> OLD.payer_consent THEN
        RAISE EXCEPTION 'LW_COURSE_EXPERIMENT_PAYER_IMMUTABLE';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER experiment_group_payer_immutable
BEFORE UPDATE ON control.experiment_groups
FOR EACH ROW EXECUTE FUNCTION control.prevent_experiment_payer_change();

CREATE VIEW control.course_experiment_windows AS
SELECT e.course_id, e.release_id, e.policy, e.revision,
       c.open_at, c.due_at, c.late_policy, c.late_until, c.time_zone
FROM control.course_experiments e
LEFT JOIN control.course_calendars c ON c.course_id = e.course_id;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='lw_control_runtime') THEN
        GRANT SELECT ON control.course_experiment_windows TO lw_control_runtime;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='lw_resource_runtime') THEN
        GRANT SELECT ON control.course_experiment_windows TO lw_resource_runtime;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='lw_evaluation_runtime') THEN
        GRANT USAGE ON SCHEMA control TO lw_evaluation_runtime;
        GRANT SELECT ON control.course_experiment_windows TO lw_evaluation_runtime;
    END IF;
END $$;
