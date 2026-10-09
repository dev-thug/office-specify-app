-- 0003_rls_roles_guards.sql — A.5 role/grant contract + append-only and
-- campaign-freeze guards the base DDL deliberately deferred ("역할 생성·권한
-- 부여·업무 함수는 별도 배포 단계").
--
-- RLS policies themselves are created in 0001 (appendix A verbatim). This
-- migration adds:
--   1. NOLOGIN runtime roles (idempotent — CREATE ROLE has no IF NOT EXISTS).
--   2. Minimal DML grants; append-only tables get no UPDATE/DELETE for
--      runtime roles (A.5). Retention deletion goes through
--      `retention_operator` + `app.retention_operator='on'` session GUC.
--   3. Guard triggers enforcing append-only and the A.6.2 freeze rules.
--
-- Runtime roles are intentionally not table owners, not superusers and not
-- BYPASSRLS — they only see rows through `app.workspace_id` (set per
-- transaction by Db::begin in crates/infrastructure).

BEGIN;

-- ---------- 1. roles ----------
DO $$
DECLARE r TEXT;
BEGIN
    FOREACH r IN ARRAY ARRAY[
        'migration_owner','api_runtime','worker_runtime',
        'report_reader','retention_operator'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
            EXECUTE format('CREATE ROLE %I NOLOGIN', r);
        END IF;
    END LOOP;
END$$;

-- ---------- 2. grants ----------
GRANT USAGE ON SCHEMA public
    TO api_runtime, worker_runtime, report_reader, retention_operator;

GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public
    TO api_runtime, worker_runtime;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO report_reader;
GRANT SELECT, DELETE ON audit_logs, approval_decisions, suppression_events,
    send_attempts TO retention_operator;

-- Append-only for normal runtime (A.5): audit_logs, approval_decisions,
-- suppression_events; send_attempts keeps UPDATE only for in-flight rows
-- (guard trigger below rejects updates once finished_at is set).
REVOKE UPDATE, DELETE ON audit_logs, approval_decisions, suppression_events
    FROM api_runtime, worker_runtime;
REVOKE DELETE ON send_attempts FROM api_runtime, worker_runtime;

-- Identity tables: login needs SELECT/INSERT/UPDATE (invite accept creates
-- users + credentials, lockout updates counters) but never DELETE.
REVOKE DELETE ON users, user_password_credentials
    FROM api_runtime, worker_runtime;

GRANT EXECUTE ON FUNCTION app_workspace_id()
    TO api_runtime, worker_runtime, report_reader, retention_operator;

-- ---------- 3. append-only guards ----------
-- Escape hatch for the audited retention path: a tx run by an operator sets
-- `SET LOCAL app.retention_operator = 'on'`. Normal runtime never sets it.
CREATE FUNCTION guard_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_setting('app.retention_operator', true) IS DISTINCT FROM 'on' THEN
        RAISE EXCEPTION 'append-only record is immutable' USING ERRCODE = '23514';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER audit_logs_append_only
    BEFORE UPDATE OR DELETE ON audit_logs
    FOR EACH ROW EXECUTE FUNCTION guard_append_only();
CREATE TRIGGER approval_decisions_append_only
    BEFORE UPDATE OR DELETE ON approval_decisions
    FOR EACH ROW EXECUTE FUNCTION guard_append_only();
CREATE TRIGGER suppression_events_append_only
    BEFORE UPDATE OR DELETE ON suppression_events
    FOR EACH ROW EXECUTE FUNCTION guard_append_only();

-- send_attempts: never DELETE; UPDATE only while in-flight (finished_at NULL).
CREATE FUNCTION guard_send_attempt() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_setting('app.retention_operator', true) = 'on' THEN
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
CREATE TRIGGER send_attempts_completed_guard
    BEFORE UPDATE OR DELETE ON send_attempts
    FOR EACH ROW EXECUTE FUNCTION guard_send_attempt();

-- ---------- 4. campaign freeze guards (A.6.2) ----------
-- The 0001 triggers only protect template_versions and outbound payload
-- snapshots. These guards finish the freeze: step mutations under a frozen
-- revision, enrollment target changes under a frozen launch, frozen launch
-- core fields, frozen revision updates, and approval target/snapshot swaps.

-- campaign_steps: no INSERT/UPDATE/DELETE once the parent revision is frozen.
CREATE FUNCTION guard_frozen_revision_steps() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE rev_status TEXT;
BEGIN
    SELECT status INTO rev_status FROM campaign_revisions
     WHERE workspace_id = COALESCE(NEW.workspace_id, OLD.workspace_id)
       AND id = COALESCE(NEW.revision_id, OLD.revision_id);
    IF rev_status = 'frozen' THEN
        RAISE EXCEPTION 'campaign revision is frozen; create a new revision'
            USING ERRCODE = '23514';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER campaign_steps_frozen_guard
    BEFORE INSERT OR UPDATE OR DELETE ON campaign_steps
    FOR EACH ROW EXECUTE FUNCTION guard_frozen_revision_steps();

-- campaign_revisions: a frozen revision is fully immutable (status stays
-- 'frozen'; content never changes — a new revision is created instead).
CREATE FUNCTION guard_frozen_revision() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'frozen' THEN
        RAISE EXCEPTION 'frozen campaign revision is immutable'
            USING ERRCODE = '23514';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER campaign_revisions_frozen_guard
    BEFORE UPDATE OR DELETE ON campaign_revisions
    FOR EACH ROW EXECUTE FUNCTION guard_frozen_revision();

-- campaign_launches: once past 'building' the structural fields (revision,
-- channel, requester, manifest, freeze timestamp) cannot change.
CREATE FUNCTION guard_frozen_launch() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status <> 'building' AND (
        NEW.revision_id          IS DISTINCT FROM OLD.revision_id OR
        NEW.delivery_channel_id  IS DISTINCT FROM OLD.delivery_channel_id OR
        NEW.requested_by         IS DISTINCT FROM OLD.requested_by OR
        NEW.manifest_hash        IS DISTINCT FROM OLD.manifest_hash OR
        NEW.frozen_at            IS DISTINCT FROM OLD.frozen_at OR
        NEW.expires_at           IS DISTINCT FROM OLD.expires_at
    ) THEN
        RAISE EXCEPTION 'frozen launch fields are immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER campaign_launches_frozen_guard
    BEFORE UPDATE ON campaign_launches
    FOR EACH ROW EXECUTE FUNCTION guard_frozen_launch();

-- campaign_enrollments: once the parent launch leaves 'building' the
-- recipient set is fixed — no INSERT/DELETE, and UPDATE may only touch
-- execution state (status, current_step_id, next_send_at, stop_reason).
CREATE FUNCTION guard_frozen_enrollment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE launch_status TEXT;
BEGIN
    SELECT status INTO launch_status FROM campaign_launches
     WHERE workspace_id = COALESCE(NEW.workspace_id, OLD.workspace_id)
       AND id = COALESCE(NEW.launch_id, OLD.launch_id);
    IF launch_status IS NULL OR launch_status = 'building' THEN
        IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'INSERT' OR TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'enrollment set is frozen after launch build'
            USING ERRCODE = '23514';
    END IF;
    IF (to_jsonb(NEW) - ARRAY['status','current_step_id','next_send_at','stop_reason'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['status','current_step_id','next_send_at','stop_reason']) THEN
        RAISE EXCEPTION 'enrollment target fields are immutable after freeze'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER campaign_enrollments_frozen_guard
    BEFORE INSERT OR UPDATE OR DELETE ON campaign_enrollments
    FOR EACH ROW EXECUTE FUNCTION guard_frozen_enrollment();

-- approval_requests: the approval target (launch/outbound/booking_change/
-- suppression), snapshot hash, requester and action type are fixed at
-- insert. Only status/version may change (A.6.2/A.6.3 — a rejected request
-- is never reused).
CREATE FUNCTION guard_approval_request_fields() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.launch_id            IS DISTINCT FROM OLD.launch_id OR
       NEW.outbound_message_id  IS DISTINCT FROM OLD.outbound_message_id OR
       NEW.booking_change_id    IS DISTINCT FROM OLD.booking_change_id OR
       NEW.suppression_id       IS DISTINCT FROM OLD.suppression_id OR
       NEW.snapshot_hash        IS DISTINCT FROM OLD.snapshot_hash OR
       NEW.requested_by         IS DISTINCT FROM OLD.requested_by OR
       NEW.action_type          IS DISTINCT FROM OLD.action_type THEN
        RAISE EXCEPTION 'approval request target and snapshot are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER approval_requests_immutable_fields
    BEFORE UPDATE ON approval_requests
    FOR EACH ROW EXECUTE FUNCTION guard_approval_request_fields();

COMMIT;
