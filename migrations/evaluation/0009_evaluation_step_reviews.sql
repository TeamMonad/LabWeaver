-- Optional advisory LLM review attached to a successful Advisory evaluation step.
-- Review JSON is informational and is intentionally separate from deterministic score fields.

ALTER TABLE evaluation_step_runs
    ADD COLUMN review_json jsonb;

ALTER TABLE evaluation_step_runs
    ADD CONSTRAINT evaluation_step_runs_review_json_object_ck
    CHECK (review_json IS NULL OR jsonb_typeof(review_json) = 'object');
