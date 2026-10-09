-- 0006_guards_grants.sql — fix-round-1 DB changes:
--   R18: campaign launch guard covers UPDATE+DELETE, forward-only status,
--        frozen columns immutable.
--   R19: enrollment/step guards evaluate BOTH the OLD parent (leaving) and
--        the NEW parent (joining) — repointing out of or into a frozen
--        parent is rejected.
--   R20: parent status lookups lock the parent row FOR SHARE so a
--        concurrent freeze commits before the guard decides.
--   R21: approval_requests transitions are pending-only, terminal states
--        immutable, 'approved' requires an approval_decisions row in the
--        same transaction; expires_at pinned.
--   R22: replace the blanket ALL TABLES grant with explicit per-table
--        grants; _sqlx_migrations, credential/token tables and
--        report_reader are tightened.
--
-- New SQL only — 0001–0004 are untouched. All objects use
-- CREATE OR REPLACE / DROP+CREATE so re-running against a DB that already
-- has 0003 applied is a clean upgrade.

BEGIN;

-- ======================= R18–R21: guard rewrites =======================

-- ---------- campaign_launches: forward-only + DELETE guard (R18) ----------
-- Status ranks: building(0) < frozen(1) < awaiting_approval(2) < approved(3)
-- < running(4) = paused(4) < completed(5) = canceled(5). Pause/resume share
-- a rank so toggling stays legal; no path leads back to 'building' or an
-- earlier rank.
CREATE OR REPLACE FUNCTION guard_frozen_launch() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    ranks CONSTANT JSONB := '{"building":0,"frozen":1,"awaiting_approval":2,"approved":3,"running":4,"paused":4,"completed":5,"canceled":5}';
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.status <> 'building' THEN
            RAISE EXCEPTION 'a launch that has left building cannot be deleted'
                USING ERRCODE = '23514';
        END IF;
        RETURN OLD;
    END IF;
    IF OLD.status <> 'building' THEN
        IF (ranks ->> NEW.status)::int < (ranks ->> OLD.status)::int THEN
            RAISE EXCEPTION 'launch status cannot move backwards'
                USING ERRCODE = '23514';
        END IF;
        IF NEW.revision_id          IS DISTINCT FROM OLD.revision_id OR
           NEW.delivery_channel_id  IS DISTINCT FROM OLD.delivery_channel_id OR
           NEW.requested_by         IS DISTINCT FROM OLD.requested_by OR
           NEW.manifest_hash        IS DISTINCT FROM OLD.manifest_hash OR
           NEW.frozen_at            IS DISTINCT FROM OLD.frozen_at OR
           NEW.expires_at           IS DISTINCT FROM OLD.expires_at THEN
            RAISE EXCEPTION 'frozen launch fields are immutable'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER campaign_launches_frozen_guard ON campaign_launches;
CREATE TRIGGER campaign_launches_frozen_guard
    BEFORE UPDATE OR DELETE ON campaign_launches
    FOR EACH ROW EXECUTE FUNCTION guard_frozen_launch();

-- ---------- campaign_steps: OLD+NEW parent, locked (R19/R20) ----------
-- The OLD-parent check blocks mutations of rows already under a frozen
-- revision (including repointing away); the NEW-parent check blocks
-- inserting into or repointing into a frozen revision. FOR SHARE serializes
-- against the freeze UPDATE so the status re-read is post-commit.
CREATE OR REPLACE FUNCTION guard_frozen_revision_steps() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    parent_status TEXT;
BEGIN
    IF TG_OP = 'UPDATE' OR TG_OP = 'DELETE' THEN
        SELECT status INTO parent_status FROM campaign_revisions
         WHERE workspace_id = OLD.workspace_id AND id = OLD.revision_id
         FOR SHARE;
        IF parent_status = 'frozen' THEN
            RAISE EXCEPTION 'campaign revision is frozen; create a new revision'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    IF TG_OP = 'INSERT' OR TG_OP = 'UPDATE' THEN
        SELECT status INTO parent_status FROM campaign_revisions
         WHERE workspace_id = NEW.workspace_id AND id = NEW.revision_id
         FOR SHARE;
        IF parent_status = 'frozen' THEN
            RAISE EXCEPTION 'campaign revision is frozen; create a new revision'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;

-- ---------- campaign_enrollments: OLD+NEW launch parent, locked (R19/R20)
-- Under a 'building' launch everything is allowed. Once the parent has left
-- 'building': no INSERT/DELETE (recipient set fixed), UPDATE may only touch
-- execution state (status, current_step_id, next_send_at, stop_reason) —
-- which also forbids repointing launch_id/revision_id/email_address_id.
CREATE OR REPLACE FUNCTION guard_frozen_enrollment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    parent_status TEXT;
BEGIN
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

-- ---------- approval_requests: transition + decision guard (R21) ----------
-- Immutable: target FKs, snapshot_hash, requested_by, action_type,
-- expires_at. Status may only leave 'pending' once, toward
-- approved/rejected/expired/revoked; terminal states are immutable.
-- 'approved' additionally requires an approval_decisions row in the same
-- transaction (inserted before the UPDATE — same-tx visibility applies).
CREATE OR REPLACE FUNCTION guard_approval_request_fields() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.launch_id            IS DISTINCT FROM OLD.launch_id OR
       NEW.outbound_message_id  IS DISTINCT FROM OLD.outbound_message_id OR
       NEW.booking_change_id    IS DISTINCT FROM OLD.booking_change_id OR
       NEW.suppression_id       IS DISTINCT FROM OLD.suppression_id OR
       NEW.snapshot_hash        IS DISTINCT FROM OLD.snapshot_hash OR
       NEW.requested_by         IS DISTINCT FROM OLD.requested_by OR
       NEW.action_type          IS DISTINCT FROM OLD.action_type OR
       NEW.expires_at           IS DISTINCT FROM OLD.expires_at THEN
        RAISE EXCEPTION 'approval request target, expiry and snapshot are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status THEN
        IF OLD.status <> 'pending' THEN
            RAISE EXCEPTION 'approval request status is terminal'
                USING ERRCODE = '23514';
        END IF;
        IF NEW.status NOT IN ('approved','rejected','expired','revoked') THEN
            RAISE EXCEPTION 'illegal approval request status transition'
                USING ERRCODE = '23514';
        END IF;
        IF NEW.status = 'approved' AND NOT EXISTS (
            SELECT 1 FROM approval_decisions d
             WHERE d.workspace_id = NEW.workspace_id
               AND d.approval_request_id = NEW.id
        ) THEN
            RAISE EXCEPTION 'approval requires a recorded decision'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

-- ======================= R22: explicit grants ===========================

-- Start from nothing on every table (including _sqlx_migrations — runtime
-- roles must never read or rewrite migration bookkeeping).
REVOKE ALL ON ALL TABLES IN SCHEMA public
    FROM api_runtime, worker_runtime, report_reader;

-- ---- global identity tables (no RLS) ----
-- workspaces/users are written by api only (bootstrap, emergency stop,
-- invite accept); never deleted by runtime roles.
GRANT SELECT, INSERT, UPDATE ON workspaces TO api_runtime;
GRANT SELECT ON workspaces TO worker_runtime, report_reader;
GRANT SELECT, INSERT, UPDATE ON users TO api_runtime;
GRANT SELECT ON users TO worker_runtime, report_reader;

-- Password credentials: api only — the worker must never read or rewrite
-- password hashes (account-takeover primitive).
GRANT SELECT, INSERT, UPDATE ON user_password_credentials TO api_runtime;

-- Sessions + invitations + service credentials: api only.
GRANT SELECT, INSERT, UPDATE ON user_sessions TO api_runtime;
GRANT SELECT, INSERT, UPDATE ON member_invitations TO api_runtime;
GRANT SELECT, INSERT, UPDATE ON service_credentials TO api_runtime;

-- principals are needed by both runtimes (Ctx resolution, job actors).
GRANT SELECT, INSERT, UPDATE ON principals TO api_runtime, worker_runtime;
GRANT SELECT ON principals TO report_reader;

-- ---- append-only / dedupe tables ----
-- audit trail + approval decisions + suppression events: write-once.
GRANT SELECT, INSERT ON audit_logs TO api_runtime, worker_runtime;
GRANT SELECT, INSERT ON approval_decisions TO api_runtime, worker_runtime;
GRANT SELECT, INSERT ON suppression_events TO api_runtime, worker_runtime;
-- consumer dedupe rows: insert-only (no UPDATE/DELETE — deleting them
-- would defeat at-least-once dedupe).
GRANT SELECT, INSERT ON processed_events TO api_runtime, worker_runtime;

-- ---- queue tables (state machines, never deleted by runtime) ----
GRANT SELECT, INSERT, UPDATE ON jobs TO api_runtime, worker_runtime;
GRANT SELECT, INSERT, UPDATE ON outbox_events TO api_runtime, worker_runtime;
GRANT SELECT, INSERT, UPDATE ON webhook_events TO api_runtime, worker_runtime;
-- idempotency_records needs DELETE: run() releases a failed 'processing'
-- claim by deleting it.
GRANT SELECT, INSERT, UPDATE, DELETE ON idempotency_records
    TO api_runtime, worker_runtime;

-- ---- request/state tables with terminal statuses — no DELETE ----
GRANT SELECT, INSERT, UPDATE ON approval_requests TO api_runtime, worker_runtime;
GRANT SELECT, INSERT, UPDATE ON send_attempts TO api_runtime, worker_runtime;
GRANT SELECT, INSERT, UPDATE ON suppressions TO api_runtime, worker_runtime;
GRANT SELECT, INSERT, UPDATE ON unsubscribe_tokens TO api_runtime, worker_runtime;
GRANT SELECT, INSERT, UPDATE ON booking_manage_tokens TO api_runtime, worker_runtime;

-- ---- full-DML business tables ----
GRANT SELECT, INSERT, UPDATE, DELETE ON
    integration_connections,
    mail_accounts,
    sender_identities,
    mail_account_grants,
    delivery_channels,
    channel_reviews,
    companies,
    contacts,
    email_addresses,
    contact_emails,
    communication_bases,
    agent_runs,
    lead_proposals,
    research_evidence,
    email_templates,
    template_versions,
    campaigns,
    campaign_revisions,
    campaign_steps,
    campaign_launches,
    campaign_enrollments,
    email_threads,
    message_drafts,
    outbound_messages,
    email_messages,
    message_participants,
    thread_read_states,
    reply_claims,
    delivery_events,
    calendar_connections,
    availability_rules,
    availability_exceptions,
    meeting_types,
    booking_links,
    bookings,
    booking_changes,
    booking_slot_holds,
    agent_actions,
    notes,
    tasks,
    activities,
    stored_objects,
    message_attachments,
    import_jobs,
    import_rows,
    sync_cursors
    TO api_runtime, worker_runtime;

-- ---- report_reader: read-only, secret-free ----
-- No credentials, sessions, invitations, capability tokens, idempotency
-- records, integration secret refs, or message bodies.
GRANT SELECT ON
    mail_accounts,
    sender_identities,
    mail_account_grants,
    delivery_channels,
    channel_reviews,
    companies,
    contacts,
    email_addresses,
    contact_emails,
    communication_bases,
    agent_runs,
    lead_proposals,
    research_evidence,
    email_templates,
    template_versions,
    campaigns,
    campaign_revisions,
    campaign_steps,
    campaign_launches,
    campaign_enrollments,
    email_threads,
    message_drafts,
    outbound_messages,
    message_participants,
    thread_read_states,
    send_attempts,
    reply_claims,
    delivery_events,
    suppressions,
    suppression_events,
    calendar_connections,
    availability_rules,
    availability_exceptions,
    meeting_types,
    booking_links,
    bookings,
    booking_changes,
    booking_slot_holds,
    approval_requests,
    approval_decisions,
    agent_actions,
    notes,
    tasks,
    activities,
    stored_objects,
    message_attachments,
    import_jobs,
    import_rows,
    webhook_events,
    sync_cursors,
    jobs,
    outbox_events,
    processed_events,
    audit_logs
    TO report_reader;

-- email_messages minus the body columns (message content is not for
-- analytics).
GRANT SELECT (
    id, workspace_id, mail_account_id, thread_id, outbound_message_id,
    direction, provider_message_id, internet_message_id, in_reply_to,
    reference_ids, subject, message_at, observed_at, classification,
    classification_reviewed_by, created_at
) ON email_messages TO report_reader;

COMMIT;
