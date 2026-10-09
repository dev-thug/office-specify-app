-- Audited, workspace-scoped privacy maintenance. API and worker roles
-- receive no additional privileges and cannot use the retention bypass.
BEGIN;
GRANT SELECT, UPDATE, DELETE ON
    contacts, contact_emails, email_addresses, communication_bases,
    research_evidence, campaign_enrollments, email_threads, message_drafts,
    outbound_messages, email_messages, message_participants, reply_claims,
    delivery_events, suppressions, suppression_events, unsubscribe_tokens,
    bookings, booking_changes, booking_manage_tokens, notes, tasks, activities,
    stored_objects, message_attachments, import_rows
TO retention_operator;
GRANT INSERT ON audit_logs, suppressions, suppression_events, outbox_events, jobs
TO retention_operator;
GRANT SELECT ON outbox_events, jobs TO retention_operator;

CREATE OR REPLACE FUNCTION protect_outbound_snapshot() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_user = 'retention_operator'
       AND current_setting('app.retention_operator', true) = 'on' THEN
        IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
        RETURN NEW;
    END IF;
    IF (to_jsonb(NEW) - ARRAY['status','not_before','accepted_at','provider_message_id','failure_code','version'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['status','not_before','accepted_at','provider_message_id','failure_code','version']) THEN
        RAISE EXCEPTION 'outbound payload is immutable; create a new message'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION guard_frozen_enrollment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    parent_status TEXT;
BEGIN
    IF current_user = 'retention_operator'
       AND current_setting('app.retention_operator', true) = 'on' THEN
        IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
        RETURN NEW;
    END IF;
    -- OLD parent (R19): DELETE under a frozen launch is rejected outright;
    -- for UPDATE, repointing away from a frozen parent (or touching any
    -- non-execution column) is rejected — execution-state updates are how
    -- the runner advances a live enrollment and must stay legal.
    IF TG_OP = 'DELETE' THEN
        SELECT status INTO parent_status FROM campaign_launches
         WHERE workspace_id = OLD.workspace_id AND id = OLD.launch_id
         FOR SHARE;
        IF parent_status IS NOT NULL AND parent_status <> 'building' THEN
            RAISE EXCEPTION 'enrollment set is frozen after launch build'
                USING ERRCODE = '23514';
        END IF;
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE' THEN
        SELECT status INTO parent_status FROM campaign_launches
         WHERE workspace_id = OLD.workspace_id AND id = OLD.launch_id
         FOR SHARE;
        IF parent_status IS NOT NULL AND parent_status <> 'building' THEN
            IF (to_jsonb(NEW) - ARRAY['status','current_step_id','next_send_at','stop_reason'])
               IS DISTINCT FROM
               (to_jsonb(OLD) - ARRAY['status','current_step_id','next_send_at','stop_reason']) THEN
                RAISE EXCEPTION 'enrollment target fields are immutable after freeze'
                    USING ERRCODE = '23514';
            END IF;
            RETURN NEW;
        END IF;
    END IF;
    -- NEW parent (R19): inserting into — or repointing into — a frozen
    -- launch is rejected.
    IF TG_OP = 'INSERT' OR TG_OP = 'UPDATE' THEN
        SELECT status INTO parent_status FROM campaign_launches
         WHERE workspace_id = NEW.workspace_id AND id = NEW.launch_id
         FOR SHARE;
        IF parent_status IS NOT NULL AND parent_status <> 'building' THEN
            IF TG_OP = 'INSERT' THEN
                RAISE EXCEPTION 'enrollment set is frozen after launch build'
                    USING ERRCODE = '23514';
            END IF;
            IF (to_jsonb(NEW) - ARRAY['status','current_step_id','next_send_at','stop_reason'])
               IS DISTINCT FROM
               (to_jsonb(OLD) - ARRAY['status','current_step_id','next_send_at','stop_reason']) THEN
                RAISE EXCEPTION 'enrollment target fields are immutable after freeze'
                    USING ERRCODE = '23514';
            END IF;
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION guard_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_user <> 'retention_operator'
       OR current_setting('app.retention_operator', true) IS DISTINCT FROM 'on' THEN
        RAISE EXCEPTION 'append-only record is immutable' USING ERRCODE = '23514';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION guard_send_attempt() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_user = 'retention_operator'
       AND current_setting('app.retention_operator', true) = 'on' THEN
        IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'send attempts are append-only' USING ERRCODE = '23514';
    END IF;
    IF OLD.finished_at IS NOT NULL THEN
        RAISE EXCEPTION 'completed send attempt is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

COMMIT;
