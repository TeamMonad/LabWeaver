-- Model token prices are explicit Credits configuration; no defaults or USD conversion.
ALTER TABLE resource.resource_rates
    ADD COLUMN model_binding text,
    ADD COLUMN max_preauthorization text;

ALTER TABLE resource.resource_rates DROP CONSTRAINT resource_rates_unit_check;
ALTER TABLE resource.resource_rates ADD CONSTRAINT resource_rates_unit_check
 CHECK (unit IN (
   'cpu_millicore_second','memory_byte_second','storage_byte_second','gpu_unit_second',
   'build_second','llm_input_token','llm_output_token','llm_cache_read_token','llm_cache_write_token'
 ));
ALTER TABLE resource.resource_rates ADD CONSTRAINT resource_rates_model_check
 CHECK ((unit IN ('llm_input_token','llm_output_token','llm_cache_read_token','llm_cache_write_token')) = (model_binding IS NOT NULL)
    AND ((unit IN ('llm_input_token','llm_output_token','llm_cache_read_token','llm_cache_write_token')) = (max_preauthorization IS NOT NULL)));
ALTER TABLE resource.resource_rates ADD CONSTRAINT resource_rates_model_binding_check
 CHECK (model_binding IS NULL OR (length(model_binding)>0 AND length(model_binding)<=256 AND model_binding= btrim(model_binding)));
ALTER TABLE resource.resource_rates ADD CONSTRAINT resource_rates_model_limit_check
 CHECK (max_preauthorization IS NULL OR max_preauthorization::numeric > 0);
CREATE INDEX resource_rates_model_window_idx ON resource.resource_rates(model_binding,effective_from)
 WHERE model_binding IS NOT NULL;
ALTER TABLE resource.credit_holds ADD COLUMN model_call_id uuid;
ALTER TABLE resource.credit_holds ADD COLUMN model_rate_snapshot jsonb CHECK(model_rate_snapshot IS NULL OR jsonb_typeof(model_rate_snapshot)='array');
CREATE UNIQUE INDEX credit_holds_model_call_idx ON resource.credit_holds(model_call_id) WHERE model_call_id IS NOT NULL;

-- Model calls are the third legitimate CreditHold target. Preserve the invariant that exactly one
-- business target is present; model_call_id is unique so retries cannot create another hold.
ALTER TABLE resource.credit_holds DROP CONSTRAINT credit_holds_target_check;
ALTER TABLE resource.credit_holds ADD CONSTRAINT credit_holds_target_check CHECK (
    num_nonnulls(request_id, build_request_id, model_call_id) = 1
    AND (build_request_id IS NULL) = (build_attempt IS NULL)
);

CREATE TABLE resource.model_cost_observations (
    event_id uuid PRIMARY KEY,
    call_id uuid NOT NULL,
    hold_id uuid NOT NULL REFERENCES resource.credit_holds(hold_id),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX model_cost_observations_call_idx ON resource.model_cost_observations(call_id,created_at,event_id);
DO $$ BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname='lw_resource_runtime') THEN
        REVOKE UPDATE, DELETE ON resource.model_cost_observations FROM lw_resource_runtime;
    END IF;
END $$;
