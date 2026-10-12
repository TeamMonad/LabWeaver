-- Control owns one managed configuration aggregate. Historical desired versions
-- and apply attempts remain available after edits, retries and failed execution.
CREATE TABLE control.platform_configuration_state (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    revision bigint NOT NULL CHECK (revision >= 0),
    contract jsonb NOT NULL
);

INSERT INTO control.platform_configuration_state (singleton, revision, contract)
VALUES (true, 0, '{"revision":0,"desired":null,"applied":null,"appliedState":"unconfigured","appliedAt":null,"appliedBundleSha256":null,"latestApply":null,"impact":{"runtimePolicyChanged":false,"builderPolicyChanged":false,"lifecyclePolicyChanged":false}}');

CREATE TABLE control.platform_configuration_versions (
    revision bigint PRIMARY KEY CHECK (revision > 0),
    contract jsonb NOT NULL
);

CREATE TABLE control.platform_configuration_applies (
    apply_id uuid PRIMARY KEY,
    desired_revision bigint NOT NULL REFERENCES control.platform_configuration_versions(revision),
    revision bigint NOT NULL CHECK (revision > 0),
    contract jsonb NOT NULL
);
