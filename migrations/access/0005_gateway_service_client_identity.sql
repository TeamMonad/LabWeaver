-- Gateway identity is the verified service-account client id, not a SPIFFE URI.
-- The JWT service identity and the Access handler remain the authentication
-- authority; this constraint only rejects empty or unbounded persisted values.
ALTER TABLE access.ssh_authorizations
    DROP CONSTRAINT ssh_authorizations_gateway_identity_check,
    ADD CONSTRAINT ssh_authorizations_gateway_identity_check
        CHECK (
            length(gateway_identity) BETWEEN 1 AND 128
            AND gateway_identity = btrim(gateway_identity)
        );
