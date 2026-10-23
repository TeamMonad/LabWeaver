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
