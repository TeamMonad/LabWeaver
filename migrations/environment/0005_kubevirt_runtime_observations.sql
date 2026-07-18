CREATE TABLE kubevirt_runtime_observations (
    environment_id uuid PRIMARY KEY,
    state text NOT NULL CHECK (state IN ('running', 'stopped', 'deleted')),
    operation_id uuid NOT NULL,
    provider_step integer NOT NULL CHECK (provider_step > 0),
    environment_generation bigint NOT NULL CHECK (environment_generation > 0),
    attempt integer NOT NULL CHECK (attempt > 0),
    request_id text NOT NULL CHECK (request_id ~ '^[0-9a-f]{64}$'),
    namespace text NOT NULL,
    virtual_machine_name text NOT NULL,
    vm_resource_generation bigint CHECK (vm_resource_generation > 0),
    observed_vm_resource_generation bigint CHECK (observed_vm_resource_generation > 0),
    vm_uid uuid,
    vmi_uid uuid,
    root_disk_uid uuid,
    guest_ip text CHECK (guest_ip IS NULL OR guest_ip::inet IS NOT NULL),
    service_cluster_ip text CHECK (service_cluster_ip IS NULL OR service_cluster_ip::inet IS NOT NULL),
    ssh_host_key_sha256 text CHECK (
        ssh_host_key_sha256 IS NULL OR ssh_host_key_sha256 ~ '^[0-9a-f]{64}$'
    ),
    observation_sha256 text NOT NULL CHECK (observation_sha256 ~ '^[0-9a-f]{64}$'),
    cleanup_evidence jsonb,
    observed_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (state = 'running'
            AND vm_resource_generation IS NOT NULL
            AND observed_vm_resource_generation >= vm_resource_generation
            AND vm_uid IS NOT NULL
            AND vmi_uid IS NOT NULL
            AND root_disk_uid IS NOT NULL
            AND guest_ip IS NOT NULL
            AND service_cluster_ip IS NOT NULL
            AND ssh_host_key_sha256 IS NOT NULL
            AND cleanup_evidence IS NULL)
        OR
        (state = 'stopped'
            AND vm_uid IS NOT NULL
            AND vmi_uid IS NULL
            AND root_disk_uid IS NOT NULL
            AND guest_ip IS NULL
            AND service_cluster_ip IS NULL
            AND ssh_host_key_sha256 IS NOT NULL
            AND cleanup_evidence IS NULL)
        OR
        (state = 'deleted'
            AND vmi_uid IS NULL
            AND guest_ip IS NULL
            AND service_cluster_ip IS NULL
            AND cleanup_evidence IS NOT NULL)
    )
);

CREATE INDEX kubevirt_runtime_observations_operation_idx
    ON kubevirt_runtime_observations (operation_id, provider_step);
