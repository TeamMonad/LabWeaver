-- Disk retention is an Environment fact, never a reuse of Resource's compute hold.
CREATE TABLE IF NOT EXISTS retained_disk_usage (
    environment_id uuid NOT NULL,
    started_at timestamptz NOT NULL,
    measured_until timestamptz NOT NULL,
    storage_bytes numeric(20,0) NOT NULL CHECK (storage_bytes > 0),
    closed_at timestamptz,
    PRIMARY KEY (environment_id, started_at),
    CHECK (measured_until >= started_at),
    CHECK (closed_at IS NULL OR closed_at = measured_until)
);
CREATE INDEX IF NOT EXISTS environment_retained_disk_expiry_idx
    ON environment_instances ((contract->'diskRetention'->>'expiresAt'))
    WHERE (observed_state='deleted' AND contract->'diskRetention'->>'state'='retained')
       OR (observed_state='stopped' AND contract->'diskRetention'->>'state'='restored'
           AND (contract->'diskRetention'->>'reactivatedAt') IS NULL);

-- Preserve every original UID checkpoint. An old destructive request may already have run;
-- it cannot be reinterpreted as retention, resumed, or granted a newly observed disk identity.
UPDATE environment_instances
SET observed_state='failed', failed_phase='deleting',
    terminal_diagnostic='LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED',
    contract=jsonb_set(
        contract || jsonb_build_object('observedState','failed','failedPhase','deleting',
            'lastDiagnosticCode','LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED'),
        '{operation}', (contract->'operation') || jsonb_build_object(
            'cleanupProtocolVersion',2,'state','failed',
            'diagnosticCode','LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED'))
WHERE contract#>>'{operation,cleanupProtocolVersion}'='1'
  AND desired_state='deleted' AND observed_state<>'deleted';

UPDATE environment_operations AS operation
SET state='failed', diagnostic='LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED',
    contract=instance.contract->'operation',
    lease_owner=NULL, lease_token=NULL, lease_expires_at=NULL, heartbeat_at=NULL,
    finished_at=COALESCE(operation.finished_at,date_trunc('milliseconds',clock_timestamp()))
FROM environment_instances AS instance
WHERE operation.operation_id=(instance.contract#>>'{operation,id}')::uuid
  AND instance.terminal_diagnostic='LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED';

-- The marker is not deletion authority for ordinary or historical operations. Their next
-- lifecycle command still captures its own exact object identities under protocol 2.
UPDATE environment_instances
SET contract=jsonb_set(contract,'{operation,cleanupProtocolVersion}','2'::jsonb)
WHERE contract#>>'{operation,cleanupProtocolVersion}'='1';
UPDATE environment_operations
SET contract=jsonb_set(contract,'{cleanupProtocolVersion}','2'::jsonb)
WHERE contract->>'cleanupProtocolVersion'='1';
