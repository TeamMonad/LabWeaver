-- ResourceQuota reconciliation must retain both the workload limit selected by the approver
-- and the exact namespace quota plan (including provider-owned overhead).

ALTER TABLE capacity_claims
    ADD COLUMN workload_cpu_millicores integer NOT NULL DEFAULT 1
        CHECK (workload_cpu_millicores > 0),
    ADD COLUMN workload_memory_bytes bigint NOT NULL DEFAULT 1
        CHECK (workload_memory_bytes > 0),
    ADD COLUMN workload_storage_bytes bigint NOT NULL DEFAULT 1
        CHECK (workload_storage_bytes > 0),
    ADD COLUMN workload_gpu_class text,
    ADD COLUMN workload_gpu_count integer,
    ADD COLUMN quota_cpu_millicores integer NOT NULL DEFAULT 1
        CHECK (quota_cpu_millicores > 0),
    ADD COLUMN quota_memory_bytes bigint NOT NULL DEFAULT 1
        CHECK (quota_memory_bytes > 0),
    ADD COLUMN quota_storage_bytes bigint NOT NULL DEFAULT 1
        CHECK (quota_storage_bytes > 0),
    ADD COLUMN quota_gpu_class text,
    ADD COLUMN quota_gpu_count integer,
    ADD CONSTRAINT capacity_claims_workload_gpu_pair
        CHECK ((workload_gpu_class IS NULL) = (workload_gpu_count IS NULL)),
    ADD CONSTRAINT capacity_claims_quota_gpu_pair
        CHECK ((quota_gpu_class IS NULL) = (quota_gpu_count IS NULL)),
    ADD CONSTRAINT capacity_claims_workload_gpu_count
        CHECK (workload_gpu_count IS NULL OR workload_gpu_count > 0),
    ADD CONSTRAINT capacity_claims_quota_gpu_count
        CHECK (quota_gpu_count IS NULL OR quota_gpu_count > 0);
