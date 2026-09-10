-- The package and object-store locators used by an Evaluation release are private runtime input.
-- They are immutable and must remain durable with the release; a later package lookup cannot
-- change the program or evaluator that an existing release executes.

ALTER TABLE evaluation_releases
    ADD COLUMN execution_binding jsonb NOT NULL
        CHECK (jsonb_typeof(execution_binding) = 'object');
