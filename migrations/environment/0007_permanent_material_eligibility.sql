-- A permanent CourseMaterial release has no material eligibility deadline. Work leases remain
-- independently bounded by Resource's lease fence and are never copied into this column.
ALTER TABLE environment_instances
    ALTER COLUMN eligibility_expires_at DROP NOT NULL;
