-- Project is the durable ownership boundary for independent Work and optional
-- course-associated teaching. Access owns memberships; Control owns metadata.

CREATE TABLE control.projects (
    project_id uuid PRIMARY KEY,
    owner_actor_id uuid NOT NULL,
    name text NOT NULL CHECK (length(trim(name)) BETWEEN 1 AND 120),
    description text CHECK (description IS NULL OR length(description) <= 2000),
    course_id uuid,
    state text NOT NULL CHECK (state IN ('active', 'archived')),
    revision bigint NOT NULL CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object')
);

CREATE INDEX projects_owner_idx ON control.projects (owner_actor_id, updated_at DESC, project_id);
CREATE INDEX projects_course_idx ON control.projects (course_id, updated_at DESC, project_id)
    WHERE course_id IS NOT NULL;
