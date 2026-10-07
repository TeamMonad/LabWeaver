-- The durable sandbox payload is decoded by Agent after every restart.  Rewrite both
-- delivered and retryable rows before the new contract is used; delivery state, event
-- identity, interval, kind, and measurement stay in the payload unchanged.
UPDATE agent.authoring_sandbox_attempts AS attempt
SET usage_payload = rewritten.payload
FROM (
    SELECT
        attempt.run_id,
        attempt.track,
        attempt.attempt_number,
        attempt.execution_generation,
        jsonb_agg(
            jsonb_set(
                item.value - 'projectId' - 'courseId' - 'requestId' - 'leaseId',
                '{target}',
                COALESCE(
                    item.value -> 'target',
                    jsonb_strip_nulls(jsonb_build_object(
                        'kind', 'resource_request',
                        'requestId', item.value -> 'requestId',
                        'leaseId', item.value -> 'leaseId'
                    ))
                ),
                true
            )
            ORDER BY item.ordinality
        ) AS payload
    FROM agent.authoring_sandbox_attempts AS attempt
    CROSS JOIN LATERAL jsonb_array_elements(attempt.usage_payload) WITH ORDINALITY AS item(value, ordinality)
    WHERE attempt.usage_payload IS NOT NULL
    GROUP BY attempt.run_id, attempt.track, attempt.attempt_number, attempt.execution_generation
) AS rewritten
WHERE attempt.run_id = rewritten.run_id
  AND attempt.track = rewritten.track
  AND attempt.attempt_number = rewritten.attempt_number
  AND attempt.execution_generation = rewritten.execution_generation;

-- Fail the migration while the transaction can still be rolled back if a historical
-- payload cannot be decoded as the new ResourceRequest target.  A null lease remains
-- valid because Experiment and some legacy Work deliveries did not carry one.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM agent.authoring_sandbox_attempts AS attempt
        CROSS JOIN LATERAL jsonb_array_elements(attempt.usage_payload) AS item(value)
        WHERE attempt.usage_payload IS NOT NULL
          AND (
              COALESCE(jsonb_typeof(item.value -> 'target'), '') <> 'object'
              OR item.value -> 'target' ->> 'kind' <> 'resource_request'
              OR COALESCE(jsonb_typeof(item.value -> 'target' -> 'requestId'), '') <> 'string'
              OR COALESCE(item.value -> 'target' ->> 'requestId', '') !~
                    '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
              OR (
                  item.value -> 'target' ? 'leaseId'
                  AND jsonb_typeof(item.value -> 'target' -> 'leaseId') NOT IN ('null', 'string')
              )
              OR (
                  jsonb_typeof(item.value -> 'target' -> 'leaseId') = 'string'
                  AND COALESCE(item.value -> 'target' ->> 'leaseId', '') !~
                        '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
              )
          )
    ) THEN
        RAISE EXCEPTION 'agent sandbox usage payload has an invalid resource target';
    END IF;
END
$$;
