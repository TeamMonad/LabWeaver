-- Control is the sole membership writer. Access consumes a current read only snapshot.
CREATE TABLE control.course_memberships (
    course_id uuid NOT NULL,
    actor_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('teacher','student','platform_admin')),
    state text NOT NULL CHECK (state IN ('active','suspended','revoked')),
    revision bigint NOT NULL CHECK (revision > 0),
    expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (course_id,actor_id,role),
    UNIQUE (course_id,actor_id,revision)
);
CREATE TABLE control.project_memberships (
    course_id uuid,
    project_id uuid NOT NULL,
    actor_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('teacher','student','platform_admin')),
    object_role text NOT NULL DEFAULT 'viewer' CHECK (object_role IN ('viewer','operator','editor','manager','owner')),
    state text NOT NULL CHECK (state IN ('active','suspended','revoked')),
    revision bigint NOT NULL CHECK (revision > 0),
    expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id,actor_id),
    UNIQUE (project_id,actor_id,revision)
);
-- Only the Control runtime writes facts. Access cannot INSERT, UPDATE or DELETE them.
DO $$ BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname='lw_access_runtime') THEN
        GRANT USAGE ON SCHEMA control TO lw_access_runtime;
        GRANT SELECT ON control.course_memberships,control.project_memberships TO lw_access_runtime;
    END IF;
END $$;

-- SSE timestamps use the same millisecond precision as the public timestamp contract.
DO $$ BEGIN
    IF to_regclass('control.sse_project_events') IS NOT NULL THEN
        ALTER TABLE control.sse_project_events ALTER COLUMN created_at SET DEFAULT date_trunc('milliseconds',clock_timestamp());
    END IF;
END $$;
