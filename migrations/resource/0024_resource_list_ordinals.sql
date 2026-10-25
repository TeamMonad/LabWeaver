-- Immutable insertion order for Resource catalogs and financial histories. These keys are
-- independent of mutable contracts, wall-clock precision, and the continued existence of anchors.
ALTER TABLE resource.resource_requests ADD COLUMN ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE;
ALTER TABLE resource.resource_leases ADD COLUMN ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE;
ALTER TABLE resource.resource_rates ADD COLUMN ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE;
ALTER TABLE resource.gpu_catalog_entries ADD COLUMN ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE;
ALTER TABLE resource.resource_charges ADD COLUMN ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE;
ALTER TABLE resource.resource_usage_records ADD COLUMN ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE;
ALTER TABLE resource.credit_ledger_entries ADD COLUMN ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE;
ALTER TABLE resource.credit_holds ADD COLUMN ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE;
CREATE INDEX resource_charges_project_page ON resource.resource_charges(project_id, ordinal DESC);
CREATE INDEX resource_usage_project_page ON resource.resource_usage_records(project_id, ordinal DESC);
CREATE INDEX credit_ledger_project_page ON resource.credit_ledger_entries(project_id, ordinal DESC);
CREATE INDEX credit_holds_project_page ON resource.credit_holds(project_id, ordinal DESC);
CREATE INDEX resource_leases_request_page ON resource.resource_leases(request_id, ordinal DESC);
