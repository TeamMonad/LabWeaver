-- v3 Resource ownership, one-shot task targets, GPU catalog capacity, and
-- calculation-only usage/charge records.

ALTER TABLE resource.resource_requests
    ALTER COLUMN course_id DROP NOT NULL,
    ALTER COLUMN project_id SET NOT NULL,
    ALTER COLUMN environment_id DROP NOT NULL,
    ALTER COLUMN release_id DROP NOT NULL,
    ALTER COLUMN release_version DROP NOT NULL,
    ADD COLUMN target_kind text NOT NULL DEFAULT 'environment',
    ADD COLUMN task_run_id uuid;

ALTER TABLE resource.resource_requests
    ADD CONSTRAINT resource_requests_target_shape CHECK (
        (target_kind = 'environment'
            AND environment_id IS NOT NULL
            AND release_id IS NOT NULL
            AND release_version IS NOT NULL
            AND task_run_id IS NULL)
        OR
        (target_kind = 'task'
            AND environment_id IS NULL
            AND release_id IS NULL
            AND release_version IS NULL
            AND task_run_id IS NOT NULL)
    );

DROP INDEX resource.resource_requests_live_without_project_key;
DROP INDEX resource.resource_requests_live_with_project_key;
CREATE UNIQUE INDEX resource_requests_live_project_key
    ON resource.resource_requests (requester_id, project_id, request_key)
    WHERE state IN ('reviewing', 'allocating', 'active', 'expiring');

ALTER TABLE resource.capacity_claims
    ADD COLUMN gpu_catalog_entry_id uuid,
    ADD COLUMN gpu_mode text,
    ADD COLUMN gpu_allocation_binding text,
    ADD CONSTRAINT capacity_claims_gpu_resolution_pair CHECK (
        (gpu_catalog_entry_id IS NULL AND gpu_mode IS NULL AND gpu_allocation_binding IS NULL)
        OR (gpu_catalog_entry_id IS NOT NULL AND gpu_mode IS NOT NULL
            AND gpu_allocation_binding IS NOT NULL AND length(trim(gpu_allocation_binding)) > 0)
    ),
    ADD CONSTRAINT capacity_claims_gpu_mode_valid CHECK (
        gpu_mode IS NULL OR gpu_mode IN ('exclusive', 'container_time_slice', 'vm_vgpu')
    );

CREATE TABLE resource.gpu_catalog_entries (
    entry_id uuid PRIMARY KEY,
    class text NOT NULL CHECK (class ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    mode text NOT NULL CHECK (mode IN ('exclusive', 'container_time_slice', 'vm_vgpu')),
    provider_binding text NOT NULL CHECK (length(trim(provider_binding)) BETWEEN 1 AND 120),
    capacity_units integer NOT NULL CHECK (capacity_units > 0),
    allocation_binding text NOT NULL CHECK (length(trim(allocation_binding)) BETWEEN 1 AND 256),
    revision bigint NOT NULL CHECK (revision > 0),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    UNIQUE (class, mode, provider_binding, revision)
);

CREATE UNIQUE INDEX gpu_catalog_active_class
    ON resource.gpu_catalog_entries (class)
    WHERE active;

CREATE TABLE resource.gpu_capacity_observations (
    observation_id uuid PRIMARY KEY,
    entry_id uuid NOT NULL REFERENCES resource.gpu_catalog_entries(entry_id),
    available_units integer NOT NULL CHECK (available_units >= 0),
    source_binding text NOT NULL CHECK (length(trim(source_binding)) BETWEEN 1 AND 256),
    observed_at timestamptz NOT NULL,
    valid_until timestamptz NOT NULL CHECK (valid_until > observed_at),
    UNIQUE (entry_id, observed_at)
);

CREATE INDEX gpu_capacity_current_idx
    ON resource.gpu_capacity_observations (entry_id, valid_until DESC, observed_at DESC);

-- A reservation is durable capacity ownership, separate from the provider's
-- eventual device binding.  Resource locks the catalog row while calculating
-- available units and keeps this row until the claim is released.
CREATE TABLE resource.gpu_capacity_reservations (
    reservation_id uuid PRIMARY KEY,
    claim_id uuid NOT NULL UNIQUE REFERENCES resource.capacity_claims(claim_id),
    entry_id uuid NOT NULL REFERENCES resource.gpu_catalog_entries(entry_id),
    units integer NOT NULL CHECK (units > 0),
    state text NOT NULL CHECK (state IN ('reserved', 'released')),
    created_at timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz
);

CREATE INDEX gpu_reservations_active_idx
    ON resource.gpu_capacity_reservations (entry_id, state)
    WHERE state = 'reserved';

CREATE TABLE resource.resource_rates (
    rate_id uuid PRIMARY KEY,
    revision bigint NOT NULL CHECK (revision > 0),
    unit text NOT NULL CHECK (unit IN (
        'cpu_millicore_second', 'memory_byte_second',
        'storage_byte_second', 'gpu_unit_second'
    )),
    unit_quantity numeric(39,0) NOT NULL CHECK (unit_quantity > 0),
    gpu_class text CHECK (gpu_class IS NULL OR gpu_class ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    gpu_mode text CHECK (gpu_mode IS NULL OR gpu_mode IN ('exclusive', 'container_time_slice', 'vm_vgpu')),
    currency text NOT NULL CHECK (currency ~ '^[A-Za-z0-9_-]{1,32}$'),
    unit_price text NOT NULL CHECK (unit_price ~ '^(0|[1-9][0-9]*)[.][0-9]{6}$'),
    effective_from timestamptz NOT NULL,
    effective_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((gpu_class IS NULL) = (gpu_mode IS NULL)),
    CHECK (effective_until IS NULL OR effective_until > effective_from),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object')
);

CREATE INDEX resource_rates_lookup_idx
    ON resource.resource_rates (unit, gpu_class, gpu_mode, effective_from DESC);

CREATE TABLE resource.resource_usage_records (
    usage_record_id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    course_id uuid,
    request_id uuid NOT NULL REFERENCES resource.resource_requests(request_id),
    lease_id uuid REFERENCES resource.resource_leases(lease_id),
    source_event_id uuid NOT NULL UNIQUE,
    kind text NOT NULL CHECK (kind IN ('compute', 'storage')),
    measured_from timestamptz NOT NULL,
    measured_until timestamptz NOT NULL,
    measurement_state text NOT NULL CHECK (measurement_state IN ('known', 'unknown')),
    cpu_millicore_seconds numeric(39,0),
    memory_byte_seconds numeric(39,0),
    storage_byte_seconds numeric(39,0),
    gpu_unit_seconds numeric(39,0),
    unknown_reason text,
    settlement text NOT NULL CHECK (settlement IN ('pending', 'settled', 'unsettled')),
    observed_at timestamptz NOT NULL,
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    CHECK (measured_until > measured_from),
    CHECK (observed_at >= measured_until),
    CHECK (
        (measurement_state = 'known'
            AND cpu_millicore_seconds IS NOT NULL AND cpu_millicore_seconds >= 0
            AND memory_byte_seconds IS NOT NULL AND memory_byte_seconds >= 0
            AND storage_byte_seconds IS NOT NULL AND storage_byte_seconds >= 0
            AND gpu_unit_seconds IS NOT NULL AND gpu_unit_seconds >= 0
            AND unknown_reason IS NULL)
        OR
        (measurement_state = 'unknown'
            AND cpu_millicore_seconds IS NULL AND memory_byte_seconds IS NULL
            AND storage_byte_seconds IS NULL AND gpu_unit_seconds IS NULL
            AND length(trim(unknown_reason)) BETWEEN 1 AND 500
            AND settlement <> 'settled')
    )
);

CREATE INDEX resource_usage_project_idx
    ON resource.resource_usage_records (project_id, kind, observed_at DESC, usage_record_id);

CREATE TABLE resource.resource_charges (
    charge_id uuid PRIMARY KEY,
    usage_record_id uuid NOT NULL REFERENCES resource.resource_usage_records(usage_record_id),
    project_id uuid NOT NULL,
    course_id uuid,
    lines jsonb NOT NULL CHECK (jsonb_typeof(lines) = 'array' AND jsonb_array_length(lines) > 0),
    currency text NOT NULL CHECK (currency ~ '^[A-Za-z0-9_-]{1,32}$'),
    total_amount text NOT NULL CHECK (total_amount ~ '^-?(0|[1-9][0-9]*)[.][0-9]{6}$'),
    settlement text NOT NULL CHECK (settlement IN ('pending', 'settled', 'unsettled')),
    adjustment_of uuid REFERENCES resource.resource_charges(charge_id),
    adjustment_reason text,
    adjusted_by uuid,
    diagnostic_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object'),
    CHECK ((adjustment_of IS NULL) = (adjustment_reason IS NULL AND adjusted_by IS NULL)),
    CHECK (adjustment_reason IS NULL OR length(trim(adjustment_reason)) BETWEEN 1 AND 500)
);

CREATE INDEX resource_charges_project_idx
    ON resource.resource_charges (project_id, created_at DESC, charge_id);

CREATE UNIQUE INDEX resource_charges_usage_base_idx
    ON resource.resource_charges (usage_record_id)
    WHERE adjustment_of IS NULL;

CREATE TABLE resource.resource_budgets (
    budget_id uuid PRIMARY KEY,
    project_id uuid NOT NULL UNIQUE,
    course_id uuid,
    currency text NOT NULL CHECK (currency ~ '^[A-Za-z0-9_-]{1,32}$'),
    limit_amount text NOT NULL CHECK (limit_amount ~ '^(0|[1-9][0-9]*)[.][0-9]{6}$'),
    warning_amount text NOT NULL CHECK (warning_amount ~ '^(0|[1-9][0-9]*)[.][0-9]{6}$'),
    spent_amount text NOT NULL CHECK (spent_amount ~ '^(0|[1-9][0-9]*)[.][0-9]{6}$'),
    revision bigint NOT NULL CHECK (revision > 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract) = 'object')
);
