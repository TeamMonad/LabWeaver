-- Durable publication claims release the database connection before broker acknowledgement.
ALTER TABLE environment.outbox_events
    ADD COLUMN publish_claim_token uuid,
    ADD COLUMN publish_claim_until timestamptz,
    ADD CONSTRAINT outbox_publication_claim_pair CHECK (
        (publish_claim_token IS NULL) = (publish_claim_until IS NULL)
    );

CREATE INDEX outbox_pending_publication_order
    ON environment.outbox_events (created_at, event_id)
    WHERE published_at IS NULL;
