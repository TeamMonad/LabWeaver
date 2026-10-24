-- A release is approved from either an Agent candidate or a human-reviewed platform image.
ALTER TABLE control.environment_template_releases
    ALTER COLUMN environment_candidate_id DROP NOT NULL,
    ALTER COLUMN candidate_revision DROP NOT NULL,
    ADD CONSTRAINT environment_template_release_source_check CHECK ((
        (environment_candidate_id IS NOT NULL AND candidate_revision IS NOT NULL
         AND COALESCE(contract->'manualApproval','null'::jsonb)='null'::jsonb)
        OR
        (environment_candidate_id IS NULL AND candidate_revision IS NULL
         AND jsonb_typeof(contract->'manualApproval')='object'
         AND contract->'manualApproval'->'spec'->>'class'='work')
    ) IS TRUE);

CREATE OR REPLACE VIEW control.runtime_authorization_configurations AS
    SELECT r.release_id,r.version AS release_version,r.project_id,r.course_id,
           r.spec_sha256,r.contract AS release_contract,
           c.contract AS candidate_contract,a.contract AS authoring_approval_contract
    FROM control.environment_template_releases r
    LEFT JOIN control.candidates c ON c.candidate_id=r.environment_candidate_id
        AND c.revision=r.candidate_revision AND c.candidate_kind='environment'
        AND c.state='validated'
        AND c.project_id=r.project_id AND c.course_id IS NOT DISTINCT FROM r.course_id
        AND c.content_sha256=r.spec_sha256
    LEFT JOIN control.authoring_approval_publications p
        ON p.environment_release_id=r.release_id AND p.state='ready'
    LEFT JOIN control.authoring_approvals a ON a.approval_id=p.approval_id
    WHERE (c.candidate_id IS NOT NULL OR
           (r.environment_candidate_id IS NULL AND r.candidate_revision IS NULL
            AND jsonb_typeof(r.contract->'manualApproval')='object'))
      AND NOT EXISTS (SELECT 1 FROM control.release_withdrawals w
        WHERE w.release_id=r.release_id AND w.release_version=r.version);
