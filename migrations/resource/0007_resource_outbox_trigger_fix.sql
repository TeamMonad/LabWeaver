-- Fix: date_trunc() requires quoted string literal for the field argument.
-- The original trigger used unquoted `milliseconds` which PostgreSQL interprets
-- as a column name rather than the string 'millisecond', causing a runtime error
-- on INSERT into resource.outbox_events and blocking all idempotency flows.

CREATE OR REPLACE FUNCTION resource.auto_publish_outbox_events()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.published_at := date_trunc('millisecond', clock_timestamp());
    RETURN NEW;
END;
$$;
