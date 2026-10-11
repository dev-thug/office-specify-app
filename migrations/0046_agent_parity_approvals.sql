-- Agent principals act with the same authority as Admin/Manager humans:
-- an active agent may record approval decisions. Credential scopes
-- (mail.approve) remain the application-level gate. Preserve snapshot,
-- version, expiry, and append-only decision guards.
CREATE OR REPLACE FUNCTION reject_self_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    req approval_requests%ROWTYPE;
    actor principals%ROWTYPE;
BEGIN
    SELECT * INTO req FROM approval_requests
      WHERE workspace_id = NEW.workspace_id AND id = NEW.approval_request_id
      FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'approval request not found' USING ERRCODE = '23503';
    END IF;
    SELECT * INTO actor FROM principals
      WHERE workspace_id = NEW.workspace_id AND id = NEW.decided_by;
    IF actor.id IS NULL OR actor.status <> 'active'
       OR NOT ((actor.kind = 'human' AND actor.role IN ('admin','manager'))
               OR actor.kind = 'agent') THEN
        RAISE EXCEPTION 'active approver required' USING ERRCODE = '23514';
    END IF;
    IF req.status <> 'pending' OR req.expires_at <= clock_timestamp()
       OR req.snapshot_hash IS DISTINCT FROM NEW.snapshot_hash
       OR req.version <> NEW.version_at_decision THEN
        RAISE EXCEPTION 'stale or invalid approval' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
