-- Recover outbound dispatch jobs whose worker lease expires across the
-- provider boundary. A still-queued outbound with no attempt is safe to
-- retry; a submitting outbound is ambiguous and must be looked up only.

BEGIN;

CREATE OR REPLACE FUNCTION reclaim_expired_job_leases() RETURNS BIGINT
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
DECLARE
    n BIGINT := 0;
    total BIGINT := 0;
BEGIN
    -- The handler had not crossed the committed submitting boundary, so
    -- this dispatch is safe to retry. Keep the normal attempt cap/backoff.
    UPDATE public.jobs j
       SET state = CASE WHEN j.attempt_count >= 5 THEN 'dead' ELSE 'retry_wait' END,
           finished_at = CASE WHEN j.attempt_count >= 5 THEN now() ELSE NULL END,
           available_at = CASE
               WHEN j.attempt_count >= 5 THEN j.available_at
               ELSE now() + make_interval(secs =>
                   (30 * power(2.0, least(greatest(j.attempt_count - 1, 0), 6)))::int)
           END,
           last_error_code = CASE
               WHEN j.attempt_count >= 5 THEN 'LEASE_EXPIRED_MAX_ATTEMPTS'
               ELSE 'LEASE_EXPIRED'
           END,
           lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
     WHERE j.state = 'running' AND j.lease_expires_at < now()
       AND j.external_effect AND j.kind = 'sending.dispatch_outbound'
       AND EXISTS (
           SELECT 1 FROM public.outbound_messages o
            WHERE o.workspace_id = j.workspace_id
              AND o.id::text = j.payload->>'outbound_message_id'
              AND o.status = 'queued'
              AND NOT EXISTS (
                  SELECT 1 FROM public.send_attempts sa
                   WHERE sa.workspace_id = o.workspace_id
                     AND sa.outbound_message_id = o.id
              )
       );
    GET DIAGNOSTICS n = ROW_COUNT;
    total := total + n;

    -- The submitting boundary committed, so the provider may have accepted
    -- the request. Fence the old job, mark the active attempt and outbound
    -- unknown, and atomically create the same event, audit, and lookup-only
    -- reconciliation work as the normal lost-owner path.
    WITH expired AS (
        UPDATE public.jobs j
           SET state = 'unknown',
               finished_at = now(),
               last_error_code = 'LEASE_EXPIRED_UNKNOWN',
               lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
         WHERE j.state = 'running' AND j.lease_expires_at < now()
           AND j.external_effect AND j.kind = 'sending.dispatch_outbound'
           AND EXISTS (
               SELECT 1 FROM public.outbound_messages o
                WHERE o.workspace_id = j.workspace_id
                  AND o.id::text = j.payload->>'outbound_message_id'
                  AND o.status = 'submitting'
           )
        RETURNING j.workspace_id, j.id AS job_id,
                  j.payload->>'outbound_message_id' AS outbound_id
    ),
    recoverable AS (
        SELECT DISTINCT ON (o.workspace_id, o.id)
               o.workspace_id, o.id AS outbound_id, o.requested_by,
               e.job_id
          FROM expired e
          JOIN public.outbound_messages o
            ON o.workspace_id = e.workspace_id
           AND o.id::text = e.outbound_id
         WHERE o.status = 'submitting'
         ORDER BY o.workspace_id, o.id, e.job_id
    ),
    attempts AS (
        UPDATE public.send_attempts sa
           SET status = 'unknown',
               error_code = 'PROVIDER_SUBMISSION_OUTCOME_UNKNOWN',
               finished_at = now()
          FROM recoverable r
         WHERE sa.workspace_id = r.workspace_id
           AND sa.outbound_message_id = r.outbound_id
           AND sa.status = 'started' AND sa.finished_at IS NULL
        RETURNING sa.id
    ),
    outbounds AS (
        UPDATE public.outbound_messages o
           SET status = 'unknown',
               failure_code = 'PROVIDER_SUBMISSION_OUTCOME_UNKNOWN',
               version = o.version + 1
          FROM recoverable r
         WHERE o.workspace_id = r.workspace_id
           AND o.id = r.outbound_id
           AND o.status = 'submitting'
        RETURNING o.workspace_id, o.id AS outbound_id, o.version,
                  r.requested_by, r.job_id
    ),
    emitted AS (
        INSERT INTO public.outbox_events
            (workspace_id, event_type, aggregate_type, aggregate_id,
             aggregate_version, payload)
        SELECT workspace_id, 'outbound.unknown', 'outbound_message',
               outbound_id, version,
               jsonb_build_object(
                   'outbound_message_id', outbound_id,
                   'reason', 'submitting_owner_lost'
               )
          FROM outbounds
        RETURNING id
    ),
    audited AS (
        INSERT INTO public.audit_logs
            (workspace_id, requested_by, job_id, action, target_type,
             target_id, request_id, details)
        SELECT workspace_id, requested_by, job_id, 'send.isolated_submitting',
               'outbound_message', outbound_id, 'job:' || job_id::text,
               jsonb_build_object('reason', 'owning job lease lost', 'outcome', 'ok')
          FROM outbounds
        RETURNING id
    ),
    reconcile_counts AS (
        SELECT o.workspace_id, o.outbound_id, count(j.id)::BIGINT AS job_count
          FROM outbounds o
          LEFT JOIN public.jobs j
            ON j.workspace_id = o.workspace_id
           AND j.kind = 'sending.reconcile_unknown'
           AND j.payload->>'outbound_message_id' = o.outbound_id::text
         GROUP BY o.workspace_id, o.outbound_id
    ),
    reconcile_jobs AS (
        INSERT INTO public.jobs
            (workspace_id, kind, payload_version, payload, available_at,
             dedupe_key, external_effect)
        SELECT workspace_id, 'sending.reconcile_unknown', 1,
               jsonb_build_object('outbound_message_id', outbound_id),
               now() + interval '5 minutes',
               'reconcile:' || outbound_id::text || ':' || job_count::text,
               false
          FROM reconcile_counts
         WHERE job_count < 12
        ON CONFLICT (workspace_id, kind, dedupe_key) DO NOTHING
        RETURNING id
    )
    SELECT count(*) INTO n FROM expired;
    total := total + n;

    -- Other external-effect jobs retain the existing no-blind-retry
    -- behavior. Dispatch jobs not matching the safe/ambiguous business
    -- states above also remain unknown rather than being resubmitted.
    UPDATE public.jobs
       SET state = 'unknown',
           finished_at = now(),
           last_error_code = 'LEASE_EXPIRED_UNKNOWN',
           lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
     WHERE state = 'running' AND lease_expires_at < now() AND external_effect;
    GET DIAGNOSTICS n = ROW_COUNT;
    total := total + n;

    -- Non-external jobs use bounded retry/backoff; MAX_ATTEMPTS is enforced
    -- here as well as in `claim` so crash paths cannot cycle forever.
    UPDATE public.jobs
       SET state = CASE WHEN attempt_count >= 5 THEN 'dead' ELSE 'retry_wait' END,
           finished_at = CASE WHEN attempt_count >= 5 THEN now() ELSE NULL END,
           available_at = CASE
               WHEN attempt_count >= 5 THEN available_at
               ELSE now() + make_interval(secs =>
                   (30 * power(2.0, least(greatest(attempt_count - 1, 0), 6)))::int)
           END,
           last_error_code = CASE
               WHEN attempt_count >= 5 THEN 'LEASE_EXPIRED_MAX_ATTEMPTS'
               ELSE 'LEASE_EXPIRED'
           END,
           lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
     WHERE state = 'running' AND lease_expires_at < now() AND NOT external_effect;
    GET DIAGNOSTICS n = ROW_COUNT;
    total := total + n;

    RETURN total;
END;
$$;

COMMIT;
