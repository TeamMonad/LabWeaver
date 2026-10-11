-- Evaluation retries decode this request after every restart.  Rewrite delivered and
-- pending rows in place, preserving the source event, interval, kind, and measurement.
UPDATE evaluation.resource_meter_deliveries AS delivery
SET request = jsonb_set(
    delivery.request - 'projectId' - 'courseId' - 'requestId' - 'leaseId',
    '{target}',
    COALESCE(
        delivery.request -> 'target',
        jsonb_strip_nulls(jsonb_build_object(
            'kind', 'resource_request',
            'requestId', delivery.request -> 'requestId',
            'leaseId', delivery.request -> 'leaseId'
        ))
    ),
    true
)
WHERE delivery.request IS NOT NULL;

-- Keep the catalog migration atomic if a row is not representable by the strict
-- RecordResourceUsageRequest decoder used by claim and idempotent enqueue paths.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM evaluation.resource_meter_deliveries AS delivery
        WHERE
            COALESCE(jsonb_typeof(delivery.request -> 'target'), '') <> 'object'
            OR delivery.request -> 'target' ->> 'kind' <> 'resource_request'
            OR COALESCE(jsonb_typeof(delivery.request -> 'target' -> 'requestId'), '') <> 'string'
            OR COALESCE(delivery.request -> 'target' ->> 'requestId', '') !~
                '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
            OR (
                delivery.request -> 'target' ? 'leaseId'
                AND jsonb_typeof(delivery.request -> 'target' -> 'leaseId') NOT IN ('null', 'string')
            )
            OR (
                jsonb_typeof(delivery.request -> 'target' -> 'leaseId') = 'string'
                AND COALESCE(delivery.request -> 'target' ->> 'leaseId', '') !~
                    '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
            )
    ) THEN
        RAISE EXCEPTION 'evaluation resource delivery has an invalid resource target';
    END IF;
END
$$;
