-- Course and invitation facts stay in Control. Access consumes current member rows only.
ALTER TABLE control.course_memberships ADD COLUMN object_role text NOT NULL DEFAULT 'participant'
    CHECK (object_role IN ('instructor','teaching_assistant','participant'));
UPDATE control.course_memberships SET object_role='instructor' WHERE role='teacher';
ALTER TABLE control.course_memberships DROP CONSTRAINT course_memberships_pkey;
-- Conflicting old role rows must fail. Do not choose or discard a member fact silently.
ALTER TABLE control.course_memberships ADD PRIMARY KEY (course_id,actor_id);

CREATE TABLE control.membership_invitations (
    invitation_id uuid PRIMARY KEY,
    scope_kind text NOT NULL CHECK (scope_kind IN ('project','course')),
    scope_id uuid NOT NULL,
    recipient_actor_id uuid NOT NULL,
    created_by uuid NOT NULL,
    issuer_membership_revision bigint CHECK (issuer_membership_revision > 0),
    target_membership_revision bigint CHECK (target_membership_revision > 0),
    state text NOT NULL CHECK (state IN ('pending','accepted','revoked')),
    revision bigint NOT NULL CHECK (revision > 0),
    contract jsonb NOT NULL CHECK (jsonb_typeof(contract)='object'),
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT date_trunc('milliseconds',clock_timestamp())
);
CREATE INDEX membership_invitation_recipient ON control.membership_invitations(recipient_actor_id,invitation_id);
CREATE INDEX membership_invitation_scope ON control.membership_invitations(scope_kind,scope_id,invitation_id);

-- The public timestamp contract uses milliseconds for course events too.
DO $$ BEGIN
    IF to_regclass('control.sse_events') IS NOT NULL THEN
        ALTER TABLE control.sse_events ALTER COLUMN created_at SET DEFAULT date_trunc('milliseconds',clock_timestamp());
    END IF;
END $$;
