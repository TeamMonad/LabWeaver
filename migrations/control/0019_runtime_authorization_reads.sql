-- Read projections of the existing Control authorities. These views store no second truth.
CREATE VIEW control.runtime_authorization_policy AS
    SELECT contract FROM control.platform_configuration_state WHERE singleton=true;

CREATE VIEW control.runtime_authorization_configurations AS
    SELECT r.release_id,r.version AS release_version,r.project_id,r.course_id,
           r.spec_sha256,r.contract AS release_contract,
           c.contract AS candidate_contract,a.contract AS authoring_approval_contract
    FROM control.environment_template_releases r
    JOIN control.candidates c ON c.candidate_id=r.environment_candidate_id
        AND c.revision=r.candidate_revision AND c.candidate_kind='environment'
        AND c.state='validated'
        AND c.project_id=r.project_id AND c.course_id IS NOT DISTINCT FROM r.course_id
        AND c.content_sha256=r.spec_sha256
    LEFT JOIN control.authoring_approval_publications p
        ON p.environment_release_id=r.release_id AND p.state='ready'
    LEFT JOIN control.authoring_approvals a ON a.approval_id=p.approval_id
    WHERE NOT EXISTS (SELECT 1 FROM control.release_withdrawals w
        WHERE w.release_id=r.release_id AND w.release_version=r.version);

DO $$ BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname='lw_access_runtime') THEN
        GRANT SELECT ON control.runtime_authorization_policy TO lw_access_runtime;
    END IF;
    IF EXISTS (SELECT FROM pg_roles WHERE rolname='lw_resource_runtime') THEN
        GRANT USAGE ON SCHEMA control TO lw_resource_runtime;
        GRANT SELECT ON control.runtime_authorization_policy,control.runtime_authorization_configurations,
            control.project_memberships,control.course_memberships TO lw_resource_runtime;
    END IF;
END $$;
