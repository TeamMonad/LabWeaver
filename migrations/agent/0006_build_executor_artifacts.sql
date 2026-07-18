CREATE TABLE build_executor_artifacts (
    build_request_id uuid PRIMARY KEY,
    build_identity text NOT NULL CHECK (build_identity ~ '^[0-9a-f]{64}$'),
    repository text NOT NULL CHECK (repository <> '' AND repository NOT LIKE '%@%'),
    project_name text NOT NULL CHECK (project_name ~ '^course-[a-z0-9-]+$'),
    repository_name text NOT NULL CHECK (repository_name ~ '^[a-z0-9._-]+$'),
    candidate_tag text NOT NULL CHECK (candidate_tag ~ '^candidate-[0-9a-f]{24}$'),
    digest text NOT NULL CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),
    cleaned_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE UNIQUE INDEX build_executor_artifacts_candidate_identity
    ON build_executor_artifacts(project_name, repository_name, candidate_tag);
