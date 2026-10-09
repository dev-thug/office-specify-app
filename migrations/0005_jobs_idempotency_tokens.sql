-- 0005_jobs_idempotency_tokens.sql — fix-round-1 DB changes:
--   R12: jobs.external_effect + reclaim_expired_job_leases rewrite
--        (external-effect jobs → 'unknown' for reconcile, never blind
--        re-submit; non-external → retry_wait with backoff, 'dead' at the
--        attempt cap).
--   R15: idempotency_records.locked_until — 'processing' claims carry a
--        5-minute lease so a crashed leader's key is not burned for 72h.
--   R16: SECURITY DEFINER token-resolution functions for the public
--        capability endpoints (unsubscribe, booking link, booking manage).
--        They return only (workspace_id, token_id) so the caller can open
--        the correct RLS-scoped transaction — the raw pattern established
--        by 0004. Used tokens intentionally resolve: repeat POSTs are
--        idempotent successes (T24), which the use case distinguishes by
--        reading the row's used_at in the workspace tx.
--
-- New SQL only — 0001–0004 are untouched.

BEGIN;

-- ---------- R12: external-effect marker + honest reclaim ----------

ALTER TABLE jobs
    ADD COLUMN external_effect BOOLEAN NOT NULL DEFAULT false;

CREATE OR REPLACE FUNCTION reclaim_expired_job_leases() RETURNS BIGINT
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
DECLARE
    n BIGINT;
    total BIGINT := 0;
BEGIN
    -- External-effect jobs whose lease expired are NEVER requeued: the
    -- provider may have accepted the submit (A.6.7 note, §16.1, ADR-006).
    -- They move to 'unknown' for the reconcile path instead.
    UPDATE public.jobs
       SET state = 'unknown',
           finished_at = now(),
           last_error_code = 'LEASE_EXPIRED_UNKNOWN',
           lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
     WHERE state = 'running' AND lease_expires_at < now() AND external_effect;
    GET DIAGNOSTICS n = ROW_COUNT;
    total := total + n;

    -- Non-external jobs: retry_wait with exponential backoff (the same
    -- 30s·2^(n-1) shape the runner uses), or 'dead' at the attempt cap —
    -- crash/panic paths count against MAX_ATTEMPTS like any other retry.
    UPDATE public.jobs
       SET state = CASE WHEN attempt_count >= 5 THEN 'dead' ELSE 'retry_wait' END,
           finished_at = CASE WHEN attempt_count >= 5 THEN now() ELSE NULL END,
           available_at = CASE
               WHEN attempt_count >= 5 THEN available_at
               ELSE now() + make_interval(secs => (30 * power(2.0, least(greatest(attempt_count - 1, 0), 6)))::int)
           END,
           last_error_code = CASE WHEN attempt_count >= 5 THEN 'LEASE_EXPIRED_MAX_ATTEMPTS' ELSE 'LEASE_EXPIRED' END,
           lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
     WHERE state = 'running' AND lease_expires_at < now() AND NOT external_effect;
    GET DIAGNOSTICS n = ROW_COUNT;
    total := total + n;

    RETURN total;
END;
$$;

-- ---------- R15: processing-claim lease ----------

ALTER TABLE idempotency_records
    ADD COLUMN locked_until TIMESTAMPTZ;

-- In-flight claims at migration time get a fresh 5-minute lease.
UPDATE idempotency_records
   SET locked_until = created_at + interval '5 minutes'
 WHERE state = 'processing' AND locked_until IS NULL;

-- ---------- R16: public token resolution (definer, minimal surface) ----------

CREATE FUNCTION resolve_unsubscribe_token(p_token_hash BYTEA)
RETURNS TABLE(workspace_id UUID, token_id UUID)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
    SELECT t.workspace_id, t.id
      FROM public.unsubscribe_tokens t
     WHERE t.token_hash = p_token_hash
$$;

CREATE FUNCTION resolve_booking_link_token(p_token_hash BYTEA)
RETURNS TABLE(workspace_id UUID, token_id UUID)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
    SELECT t.workspace_id, t.id
      FROM public.booking_links t
     WHERE t.token_hash = p_token_hash
$$;

CREATE FUNCTION resolve_booking_manage_token(p_token_hash BYTEA)
RETURNS TABLE(workspace_id UUID, token_id UUID)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
    SELECT t.workspace_id, t.id
      FROM public.booking_manage_tokens t
     WHERE t.token_hash = p_token_hash
$$;

REVOKE ALL ON FUNCTION resolve_unsubscribe_token(BYTEA) FROM PUBLIC;
REVOKE ALL ON FUNCTION resolve_booking_link_token(BYTEA) FROM PUBLIC;
REVOKE ALL ON FUNCTION resolve_booking_manage_token(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION resolve_unsubscribe_token(BYTEA) TO api_runtime;
GRANT EXECUTE ON FUNCTION resolve_booking_link_token(BYTEA) TO api_runtime;
GRANT EXECUTE ON FUNCTION resolve_booking_manage_token(BYTEA) TO api_runtime;

COMMIT;
