-- The allocation binding is the provider resource key observed by the current
-- Kubernetes capacity observer.  provider_binding selects the executor that
-- is allowed to operate it; it does not identify a separate physical pool.
-- Keep one active catalog product per allocation binding, even when an
-- operator tries to register it under another provider.
CREATE UNIQUE INDEX gpu_catalog_active_pool
    ON resource.gpu_catalog_entries (allocation_binding)
    WHERE active;
