-- Keyset pagination and actor-scoped project request reads.
CREATE INDEX resource_requests_project_created_request_idx
    ON resource.resource_requests (project_id, created_at DESC, request_id DESC);

CREATE INDEX resource_requests_project_requester_created_request_idx
    ON resource.resource_requests (project_id, requester_id, created_at DESC, request_id DESC);
