-- 0007_fix_round2.sql — fix-round-2 DB changes:
--   F1: runnable_job_workspaces(kinds) — the runner only sees workspaces
--       holding a runnable job of a kind it actually handles. Jobs of
--       unregistered kinds stay invisible to it: never claimed and never
--       dead-lettered by a worker that lacks the handler (previously a
--       subset-of-handlers worker or a rolling deploy adding a new kind
--       could NO_HANDLER-dead-letter real jobs).
--   F2: guard_approval_request_fields — 'approved' now requires an
--       approval_decisions row with decision='approved' in the same tx,
--       'rejected' requires decision='rejected'; any-row existence no
--       longer approves a request over a recorded rejection.
--
-- New SQL only — 0001–0006 are untouched.

BEGIN;

-- ---------- F1: kind-scoped runnable-job scan ----------
-- Signature change: drop the zero-arg function, then create the kind-
-- filtered variant and re-grant EXECUTE to the runtime roles only.
DROP FUNCTION runnable_job_workspaces();

CREATE FUNCTION runnable_job_workspaces(kinds TEXT[]) RETURNS SETOF UUID
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
    SELECT DISTINCT workspace_id FROM public.jobs
     WHERE state IN ('queued','retry_wait') AND available_at <= now()
       AND kind = ANY(kinds)
$$;

REVOKE ALL ON FUNCTION runnable_job_workspaces(TEXT[]) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION runnable_job_workspaces(TEXT[])
    TO api_runtime, worker_runtime;

-- ---------- F2: decision must match the transition ----------
-- 0006's guard accepted ANY approval_decisions row for 'approved', so a
-- recorded rejection could still be flipped to approved. Now the decision
-- must equal the target status for both outcomes; 'expired'/'revoked'
-- remain decision-free operator/system transitions.
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
        -- approved/rejected require a same-tx decision row whose decision
        -- MATCHES the outcome (approval_decisions is UNIQUE per request).
        IF NEW.status IN ('approved','rejected') AND NOT EXISTS (
            SELECT 1 FROM approval_decisions d
             WHERE d.workspace_id = NEW.workspace_id
               AND d.approval_request_id = NEW.id
               AND d.decision = NEW.status
        ) THEN
            RAISE EXCEPTION 'approval request status requires a matching recorded decision'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

COMMIT;
