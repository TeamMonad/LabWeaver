-- The VM can be running before its guest SSH server accepts handshakes.
-- Missing host-key evidence is unknown, never a fabricated healthy SSH endpoint.
ALTER TABLE environment.kubevirt_runtime_observations
    DROP CONSTRAINT kubevirt_runtime_observations_check,
    ADD CONSTRAINT kubevirt_runtime_observations_readiness_check CHECK (
        (state = 'running'
            AND vm_resource_generation IS NOT NULL
            AND observed_vm_resource_generation >= vm_resource_generation
            AND vm_uid IS NOT NULL AND vmi_uid IS NOT NULL AND root_disk_uid IS NOT NULL
            AND guest_ip IS NOT NULL AND service_cluster_ip IS NOT NULL
            AND cleanup_evidence IS NULL)
        OR
        (state = 'stopped'
            AND vm_uid IS NOT NULL AND vmi_uid IS NULL AND root_disk_uid IS NOT NULL
            AND guest_ip IS NULL AND service_cluster_ip IS NULL
            AND cleanup_evidence IS NULL)
        OR
        (state = 'deleted'
            AND vmi_uid IS NULL AND guest_ip IS NULL AND service_cluster_ip IS NULL
            AND cleanup_evidence IS NOT NULL)
    );
