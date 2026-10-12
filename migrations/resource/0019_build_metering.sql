-- No rate or spendable balance is seeded. BuildKit has its own real target.
ALTER TABLE resource.resource_rates DROP CONSTRAINT resource_rates_unit_check;
ALTER TABLE resource.resource_rates ADD COLUMN builder_binding text;
ALTER TABLE resource.resource_rates ADD CONSTRAINT resource_rates_unit_check
 CHECK (unit IN ('cpu_millicore_second','memory_byte_second','storage_byte_second','gpu_unit_second','build_second'));
ALTER TABLE resource.resource_rates ADD CONSTRAINT resource_rates_builder_check
 CHECK ((unit='build_second')=(builder_binding IS NOT NULL));
CREATE INDEX resource_rates_builder_window_idx ON resource.resource_rates(builder_binding,effective_from) WHERE unit='build_second';

ALTER TABLE resource.credit_holds ALTER COLUMN request_id DROP NOT NULL;
ALTER TABLE resource.credit_holds ADD COLUMN build_request_id uuid;
ALTER TABLE resource.credit_holds ADD COLUMN build_attempt integer CHECK(build_attempt>0);
ALTER TABLE resource.credit_holds ADD CONSTRAINT credit_holds_target_check CHECK (
 (request_id IS NOT NULL AND build_request_id IS NULL AND build_attempt IS NULL)
 OR (request_id IS NULL AND build_request_id IS NOT NULL AND build_attempt IS NOT NULL));
CREATE UNIQUE INDEX credit_holds_build_idx ON resource.credit_holds(build_request_id,build_attempt) WHERE build_request_id IS NOT NULL;

CREATE TABLE resource.build_metering_attempts (
 build_request_id uuid NOT NULL,
 attempt integer NOT NULL CHECK(attempt>0),
 project_id uuid NOT NULL,
 owner_client_id text NOT NULL,
 lease_token uuid NOT NULL,
 execution_deadline timestamptz,
 build_stage_request_id text,
 execution_authorized boolean NOT NULL DEFAULT false,
 command jsonb NOT NULL,
 estimate jsonb NOT NULL,
 hold_id uuid NOT NULL UNIQUE REFERENCES resource.credit_holds(hold_id) DEFERRABLE INITIALLY DEFERRED,
 measurement jsonb,
 first_unknown_observation jsonb,
 charged_lines jsonb,
 charged_total jsonb,
 query_after timestamptz NOT NULL DEFAULT now(),
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(build_request_id,attempt)
);
ALTER TABLE resource.credit_holds ADD CONSTRAINT credit_holds_build_fk
 FOREIGN KEY(build_request_id,build_attempt) REFERENCES resource.build_metering_attempts(build_request_id,attempt) DEFERRABLE INITIALLY DEFERRED;
CREATE INDEX build_metering_project_idx ON resource.build_metering_attempts(project_id,created_at DESC);
CREATE INDEX build_metering_unknown_query_idx ON resource.build_metering_attempts(query_after) WHERE charged_total IS NULL AND first_unknown_observation IS NOT NULL;
CREATE FUNCTION resource.build_metering_immutable_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF (NEW.build_request_id,NEW.attempt,NEW.project_id,NEW.owner_client_id,NEW.lease_token,NEW.command,NEW.estimate,NEW.hold_id,NEW.created_at)
 IS DISTINCT FROM (OLD.build_request_id,OLD.attempt,OLD.project_id,OLD.owner_client_id,OLD.lease_token,OLD.command,OLD.estimate,OLD.hold_id,OLD.created_at)
 OR (OLD.first_unknown_observation IS NOT NULL AND NEW.first_unknown_observation IS DISTINCT FROM OLD.first_unknown_observation)
 OR (OLD.charged_total IS NOT NULL AND (NEW.charged_total,NEW.charged_lines,NEW.measurement) IS DISTINCT FROM (OLD.charged_total,OLD.charged_lines,OLD.measurement)) THEN
  RAISE EXCEPTION 'LW_BUILD_COST_IMMUTABLE';
 END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER build_metering_identity BEFORE UPDATE ON resource.build_metering_attempts FOR EACH ROW EXECUTE FUNCTION resource.build_metering_immutable_identity();
