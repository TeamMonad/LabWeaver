-- Received ciphertext facts and administrator-declared settings bindings.
-- Registration is immutable. Applying remains a separate administrator action.
CREATE TABLE control.platform_configuration_bundles (
    registered_at_revision bigint PRIMARY KEY CHECK (registered_at_revision > 0),
    desired_revision bigint NOT NULL REFERENCES control.platform_configuration_versions(revision),
    contract jsonb NOT NULL
);

CREATE INDEX platform_configuration_bundles_desired_revision
ON control.platform_configuration_bundles (desired_revision, registered_at_revision DESC);
