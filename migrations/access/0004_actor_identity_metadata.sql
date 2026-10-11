-- Keep issuer metadata alongside the opaque actor identity so membership
-- views can be understood without exposing provider subjects.

ALTER TABLE access.actors
    ADD COLUMN username text,
    ADD COLUMN display_name text,
    ADD CONSTRAINT actors_username_nonempty
        CHECK (username IS NULL OR btrim(username) <> ''),
    ADD CONSTRAINT actors_display_name_nonempty
        CHECK (display_name IS NULL OR btrim(display_name) <> '');

-- Control may render membership metadata but must not mutate Access identity
-- rows; Access remains the sole actor metadata writer.
GRANT USAGE ON SCHEMA access TO lw_control_runtime;
GRANT SELECT ON access.actors TO lw_control_runtime;
