-- 0004_scan_functions.sql — narrow cross-workspace scans for the worker
-- runner and outbox dispatcher.
--
-- Every workspace table uses FORCE ROW LEVEL SECURITY keyed on
-- `app.workspace_id`, so no runtime session can enumerate "workspaces with
-- pending work" — with the GUC unset the policies match zero rows, even for
-- the table owner. Runtime roles deliberately have no BYPASSRLS (A.4 note),
-- and granting it would defeat workspace isolation.
--
-- These SECURITY DEFINER functions are owned by the migration identity (a
-- superuser, which bypasses RLS) and expose the minimum surface: workspace
-- ids for scans and the lease-reclaim update. All row reads/writes still
-- happen inside per-workspace RLS transactions (Db::begin), so workspace
-- isolation for actual payloads is unchanged.

BEGIN;

-- Workspaces with at least one runnable job (A.6.7 runner poll).
CREATE FUNCTION runnable_job_workspaces() RETURNS SETOF UUID
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
    SELECT DISTINCT workspace_id FROM public.jobs
     WHERE state IN ('queued','retry_wait') AND available_at <= now()
$$;

-- Workspaces with unpublished outbox events (A.6.6 dispatcher poll).
CREATE FUNCTION pending_outbox_workspaces() RETURNS SETOF UUID
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
    SELECT DISTINCT workspace_id FROM public.outbox_events
     WHERE published_at IS NULL
$$;

-- Crash recovery: expired leases return to 'queued'. attempt_count was
-- already incremented at claim time so MAX_ATTEMPTS still bounds retries.
CREATE FUNCTION reclaim_expired_job_leases() RETURNS BIGINT
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
DECLARE
    n BIGINT;
BEGIN
    UPDATE public.jobs
       SET state = 'queued', lease_owner = NULL,
           lease_token = NULL, lease_expires_at = NULL
     WHERE state = 'running' AND lease_expires_at < now();
    GET DIAGNOSTICS n = ROW_COUNT;
    RETURN n;
END;
$$;

-- Functions are EXECUTE-to-PUBLIC by default; restrict to runtime roles.
REVOKE ALL ON FUNCTION runnable_job_workspaces() FROM PUBLIC;
REVOKE ALL ON FUNCTION pending_outbox_workspaces() FROM PUBLIC;
REVOKE ALL ON FUNCTION reclaim_expired_job_leases() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION runnable_job_workspaces() TO api_runtime, worker_runtime;
GRANT EXECUTE ON FUNCTION pending_outbox_workspaces() TO api_runtime, worker_runtime;
GRANT EXECUTE ON FUNCTION reclaim_expired_job_leases() TO api_runtime, worker_runtime;

COMMIT;
