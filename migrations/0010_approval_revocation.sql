-- An approval may be withdrawn before its queued side effect starts.
-- Application transactions check and cancel the concrete target/job;
-- all target/snapshot immutability and matching-decision guards remain.
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
        IF OLD.status <> 'pending'
           AND NOT (OLD.status = 'approved' AND NEW.status = 'revoked') THEN
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
