-- The Evaluation track now generates a per-experiment evaluation runner build
-- context alongside the environment build context.

ALTER TABLE agent.generated_artifacts
    DROP CONSTRAINT IF EXISTS generated_artifacts_artifact_kind_check,
    ADD CONSTRAINT generated_artifacts_artifact_kind_check
        CHECK (artifact_kind IN ('build_context', 'work_script', 'verification_script', 'evaluation_runner_build_context'));
