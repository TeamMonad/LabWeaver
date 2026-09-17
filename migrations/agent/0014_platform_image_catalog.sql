-- Administrator-curated platform image catalog for sandbox base images and VM templates.
--
-- One row pins one reviewed image identity per (kind, binding). Registrations resolve a tag
-- reference once and persist the digest; repins and disables are conditional on the observed
-- digest so a stale administrator never overwrites a newer pin. Every change appends an audit
-- row in the same transaction, and entries referenced by a release are disabled, never deleted.
CREATE TABLE agent.platform_image_catalog (
    catalog_id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('container', 'virtual_machine')),
    binding text NOT NULL CHECK (binding ~ '^[a-z0-9][a-z0-9._-]{0,127}$'),
    source_reference text NOT NULL CHECK (length(source_reference) BETWEEN 1 AND 512),
    resolved_digest text NOT NULL CHECK (resolved_digest ~ '^sha256:[0-9a-f]{64}$'),
    media_type text NOT NULL CHECK (length(media_type) BETWEEN 1 AND 255),
    size_bytes bigint NOT NULL CHECK (size_bytes > 0),
    status text NOT NULL CHECK (status IN ('active', 'disabled')),
    trust_revision bigint NOT NULL CHECK (trust_revision > 0),
    repin_generation bigint NOT NULL DEFAULT 1 CHECK (repin_generation > 0),
    created_by uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    pinned_at timestamptz NOT NULL,
    UNIQUE (kind, binding)
);

CREATE INDEX platform_image_catalog_active_idx
    ON agent.platform_image_catalog (kind, binding)
    WHERE status = 'active';

CREATE TABLE agent.platform_image_catalog_audit (
    audit_id uuid PRIMARY KEY,
    catalog_id uuid NOT NULL REFERENCES agent.platform_image_catalog (catalog_id),
    action text NOT NULL CHECK (action IN ('registered', 'repinned', 'disabled')),
    from_digest text CHECK (from_digest IS NULL OR from_digest ~ '^sha256:[0-9a-f]{64}$'),
    to_digest text CHECK (to_digest IS NULL OR to_digest ~ '^sha256:[0-9a-f]{64}$'),
    repin_generation bigint NOT NULL CHECK (repin_generation > 0),
    actor_id uuid NOT NULL,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 512),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX platform_image_catalog_audit_entry_idx
    ON agent.platform_image_catalog_audit (catalog_id, created_at, audit_id);
