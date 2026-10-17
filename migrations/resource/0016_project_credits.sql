-- Resource-owned project Credits. No historical charge creates a wallet debit.
CREATE TABLE resource.credit_accounts (
    project_id uuid PRIMARY KEY,
    balance numeric(38,0) NOT NULL DEFAULT 0,
    reserved numeric(38,0) NOT NULL DEFAULT 0,
    spent numeric(38,0) NOT NULL DEFAULT 0,
    platform_absorbed numeric(38,0) NOT NULL DEFAULT 0,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    CHECK (balance >= reserved AND reserved >= 0 AND spent >= 0 AND platform_absorbed >= 0)
);
CREATE TABLE resource.credit_holds (
    hold_id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES resource.credit_accounts(project_id),
    request_id uuid NOT NULL REFERENCES resource.resource_requests(request_id),
    business_key text NOT NULL CHECK (length(business_key) BETWEEN 1 AND 128),
    owner_client_id text NOT NULL,
    amount numeric(38,0) NOT NULL CHECK (amount > 0),
    remaining numeric(38,0) NOT NULL CHECK (remaining >= 0 AND remaining <= amount),
    state text NOT NULL CHECK (state IN ('pending','unknown','settled','released')),
    reason text,
    measurement_unknown boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (request_id,business_key),
    CHECK (state NOT IN ('settled','released') OR remaining=0)
);
CREATE INDEX credit_holds_project_idx ON resource.credit_holds(project_id,state);
CREATE TABLE resource.credit_ledger_entries (
    entry_id uuid PRIMARY KEY,
    operation_id uuid NOT NULL,
    project_id uuid REFERENCES resource.credit_accounts(project_id),
    hold_id uuid REFERENCES resource.credit_holds(hold_id),
    reversal_of uuid UNIQUE REFERENCES resource.credit_ledger_entries(entry_id),
    balance_delta numeric(38,0) NOT NULL,
    reserved_delta numeric(38,0) NOT NULL,
    spent_delta numeric(38,0) NOT NULL,
    platform_absorbed numeric(38,0) NOT NULL CHECK (platform_absorbed >= 0),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract)='object'),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX credit_ledger_project_idx ON resource.credit_ledger_entries(project_id,created_at DESC,entry_id DESC);
CREATE FUNCTION resource.credit_ledger_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'LW_CREDITS_LEDGER_IMMUTABLE';
END;
$$;
CREATE TRIGGER credit_ledger_immutable BEFORE UPDATE OR DELETE ON resource.credit_ledger_entries
FOR EACH ROW EXECUTE FUNCTION resource.credit_ledger_immutable();
CREATE TRIGGER credit_ledger_no_truncate BEFORE TRUNCATE ON resource.credit_ledger_entries
FOR EACH STATEMENT EXECUTE FUNCTION resource.credit_ledger_immutable();
-- A transaction cannot commit a single-sided balance movement.
CREATE FUNCTION resource.credit_operation_balanced() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT sum(balance_delta) FROM resource.credit_ledger_entries
        WHERE operation_id=NEW.operation_id) <> 0 THEN
        RAISE EXCEPTION 'LW_CREDITS_OPERATION_UNBALANCED';
    END IF;
    RETURN NEW;
END;
$$;
CREATE CONSTRAINT TRIGGER credit_operation_balanced AFTER INSERT ON resource.credit_ledger_entries
DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION resource.credit_operation_balanced();
