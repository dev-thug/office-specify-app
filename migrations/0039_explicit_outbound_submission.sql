BEGIN;

-- A queued outbound may cross the provider boundary only when a current,
-- server-minted submission action bound it to its dispatch job.
ALTER TABLE outbound_messages
    ADD COLUMN explicit_submission_at TIMESTAMPTZ;

-- Old approvals used to queue mail directly. Return only unambiguous,
-- unsent rows to draft; submitting/unknown and terminal history are kept.
WITH legacy_unsent AS (
    UPDATE outbound_messages
       SET status = 'draft',
           not_before = NULL,
           failure_code = NULL,
           version = version + 1
     WHERE status IN ('queued', 'claimed')
       AND explicit_submission_at IS NULL
     RETURNING workspace_id, id
)
UPDATE jobs AS j
   SET dedupe_key = 'legacy_dispatch:' || (j.payload->>'outbound_message_id') || ':' || j.id::text,
       state = CASE WHEN j.state IN ('queued', 'retry_wait') THEN 'canceled' ELSE j.state END,
       finished_at = CASE WHEN j.state IN ('queued', 'retry_wait') THEN now() ELSE j.finished_at END,
       last_error_code = CASE WHEN j.state IN ('queued', 'retry_wait')
                              THEN 'EXPLICIT_SUBMISSION_REQUIRED' ELSE j.last_error_code END,
       lease_owner = CASE WHEN j.state IN ('queued', 'retry_wait') THEN NULL ELSE j.lease_owner END,
       lease_token = CASE WHEN j.state IN ('queued', 'retry_wait') THEN NULL ELSE j.lease_token END,
       lease_expires_at = CASE WHEN j.state IN ('queued', 'retry_wait') THEN NULL ELSE j.lease_expires_at END
  FROM legacy_unsent AS o
 WHERE j.workspace_id = o.workspace_id
   AND j.kind = 'sending.dispatch_outbound'
   AND j.payload->>'outbound_message_id' = o.id::text;

CREATE OR REPLACE FUNCTION protect_outbound_snapshot() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_user = 'retention_operator'
       AND current_setting('app.retention_operator', true) = 'on' THEN
        IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
        RETURN NEW;
    END IF;

    IF OLD.explicit_submission_at IS NOT NULL
       AND NEW.explicit_submission_at IS DISTINCT FROM OLD.explicit_submission_at THEN
        RAISE EXCEPTION 'explicit submission marker is immutable'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.explicit_submission_at IS NULL
       AND NEW.explicit_submission_at IS NOT NULL
       AND NEW.status <> 'queued' THEN
        RAISE EXCEPTION 'explicit submission marker may only be set while queueing'
            USING ERRCODE = '23514';
    END IF;

    IF (to_jsonb(NEW) - ARRAY['status','not_before','accepted_at','provider_message_id','failure_code','version','explicit_submission_at'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['status','not_before','accepted_at','provider_message_id','failure_code','version','explicit_submission_at']) THEN
        RAISE EXCEPTION 'outbound payload is immutable; create a new message'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

COMMIT;
