-- Read only. Supply reviewed test UUIDs through psql -v parameters.
SELECT frozen_submission_id, state, current_attempt, completed_at
FROM evaluation.submission_freeze_requests
WHERE frozen_submission_id = :'submission_id'::uuid
  AND project_id = :'project_id'::uuid;

SELECT frozen_submission_id, attempt, state, diagnostic_code,
       object_key, object_version, object_sha256, cleanup_verified
FROM evaluation.submission_freeze_attempts
WHERE frozen_submission_id = :'submission_id'::uuid
ORDER BY attempt;

SELECT manifest_sha256, content_sha256, object_key, object_version, contract
FROM evaluation.frozen_submissions
WHERE frozen_submission_id = :'submission_id'::uuid
  AND project_id = :'project_id'::uuid
  AND contract->>'actorId' = :'actor_id';

SELECT event_id, subject, published_at
FROM evaluation.outbox_events
WHERE aggregate_id = :'submission_id'::uuid;
