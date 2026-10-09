-- 0007_fix_round2.sql — fix-round-2 DB changes:
--   F1: runnable_job_workspaces(kinds) — the runner only sees workspaces
--       holding a runnable job of a kind it actually handles. Jobs of
--       unregistered kinds stay invisible to it: never claimed and never
--       dead-lettered by a worker that lacks the handler (previously a
--       subset-of-handlers worker or a rolling deploy adding a new kind
--       could NO_HANDLER-dead-letter real jobs).
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

COMMIT;
