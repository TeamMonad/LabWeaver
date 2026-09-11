-- Image policy evaluation is no longer an Agent-owned artifact fact.
-- Existing installations must drop the obsolete column as part of the same
-- contract transition; an empty JSON object is not a compatibility value.
ALTER TABLE agent.image_artifacts
    DROP CONSTRAINT IF EXISTS image_artifacts_policy_evaluation_object,
    DROP COLUMN IF EXISTS policy_evaluation;
