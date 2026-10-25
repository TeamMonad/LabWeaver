-- Only notification metadata is projected. No runtime or ledger commands originate here.
CREATE TABLE control.notification_sources (
    aggregate_id uuid PRIMARY KEY,
    sequence bigint NOT NULL,
    event_id uuid NOT NULL
);
CREATE TABLE control.notification_inbox (
    event_id uuid PRIMARY KEY,
    payload_sha256 text NOT NULL,
    received_at timestamptz NOT NULL DEFAULT date_trunc('milliseconds',clock_timestamp())
);
CREATE TABLE control.notifications (
    ordinal bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    notification_id uuid PRIMARY KEY,
    event_id uuid NOT NULL,
    recipient_id uuid NOT NULL,
    project_id uuid NOT NULL,
    course_id uuid,
    aggregate_id uuid NOT NULL,
    reminder_seconds bigint NOT NULL DEFAULT 0,
    due_at timestamptz NOT NULL,
    deadline_at timestamptz,
    cancelled boolean NOT NULL DEFAULT false,
    read_at timestamptz,
    contract jsonb NOT NULL,
    email_state text NOT NULL DEFAULT 'disabled' CHECK (email_state IN ('disabled','pending','sending','sent','failed','exhausted','no_verified_email','delivery_unknown')),
    email_attempts smallint NOT NULL DEFAULT 0 CHECK (email_attempts BETWEEN 0 AND 5),
    email_next_at timestamptz,
    email_claim_id uuid,
    email_diagnostic text,
    UNIQUE(event_id,recipient_id,reminder_seconds)
);
CREATE INDEX notifications_recipient_cursor ON control.notifications(recipient_id,ordinal DESC) WHERE NOT cancelled;
CREATE INDEX notifications_email_queue ON control.notifications(email_next_at) WHERE email_state IN ('pending','failed','sending');
CREATE UNIQUE INDEX notifications_lease_window ON control.notifications(aggregate_id,reminder_seconds,deadline_at) NULLS NOT DISTINCT WHERE reminder_seconds<>0;
