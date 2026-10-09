BEGIN;

-- Keep a uniform workspace DML grant for the runtimes. The immutable guard
-- installed in 0022 still rejects every attempt to change launch values.
GRANT UPDATE ON marketing_launch_values TO api_runtime, worker_runtime;

CREATE OR REPLACE FUNCTION guard_marketing_export_snapshot() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'marketing export snapshots are immutable'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.launch_id IS DISTINCT FROM OLD.launch_id
       OR NEW.revision_id IS DISTINCT FROM OLD.revision_id
       OR NEW.step_id IS DISTINCT FROM OLD.step_id
       OR NEW.delivery_channel_id IS DISTINCT FROM OLD.delivery_channel_id
       OR NEW.connection_id IS DISTINCT FROM OLD.connection_id
       OR NEW.source_manifest_hash IS DISTINCT FROM OLD.source_manifest_hash
       OR NEW.content_hash IS DISTINCT FROM OLD.content_hash
       OR NEW.audience_hash IS DISTINCT FROM OLD.audience_hash
       OR NEW.export_hash IS DISTINCT FROM OLD.export_hash
       OR NEW.subject IS DISTINCT FROM OLD.subject
       OR NEW.body_text IS DISTINCT FROM OLD.body_text
       OR NEW.body_html IS DISTINCT FROM OLD.body_html
       OR NEW.sender_address IS DISTINCT FROM OLD.sender_address
       OR NEW.sender_name IS DISTINCT FROM OLD.sender_name
       OR NEW.field_map IS DISTINCT FROM OLD.field_map
       OR NEW.prepared_by IS DISTINCT FROM OLD.prepared_by
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
        RAISE EXCEPTION 'marketing export snapshot columns are immutable'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.state IS DISTINCT FROM OLD.state
       AND NOT (
           (OLD.state = 'preparing' AND NEW.state IN ('syncing','needs_review','rejected','canceled'))
           OR (OLD.state = 'syncing' AND NEW.state IN ('ready','needs_review','unknown','rejected','canceled'))
           OR (OLD.state = 'needs_review' AND NEW.state IN ('syncing','canceled'))
           OR (OLD.state = 'ready' AND NEW.state IN ('submitting','needs_review','canceled'))
           OR (OLD.state = 'submitting' AND NEW.state IN ('in_progress','completed','unknown','rejected','needs_review'))
           OR (OLD.state = 'in_progress' AND NEW.state IN ('completed','needs_review'))
           OR (OLD.state = 'unknown' AND NEW.state IN ('in_progress','completed','rejected','needs_review'))
       ) THEN
        RAISE EXCEPTION 'invalid marketing export state transition: % -> %', OLD.state, NEW.state
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION guard_marketing_operation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'marketing operation journal rows are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.export_id IS DISTINCT FROM OLD.export_id
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.correlation_key IS DISTINCT FROM OLD.correlation_key
       OR NEW.request_hash IS DISTINCT FROM OLD.request_hash
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'marketing operation request identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.provider_id IS NOT NULL
       AND NEW.provider_id IS DISTINCT FROM OLD.provider_id THEN
        RAISE EXCEPTION 'marketing operation provider ID cannot be cleared or replaced'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.status IN ('completed','rejected') AND (
        NEW.status IS DISTINCT FROM OLD.status
        OR NEW.provider_id IS DISTINCT FROM OLD.provider_id
        OR NEW.error_code IS DISTINCT FROM OLD.error_code
        OR NEW.completed_at IS DISTINCT FROM OLD.completed_at
    ) THEN
        RAISE EXCEPTION 'terminal marketing operation results are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.status = 'unknown'
       AND NEW.status NOT IN ('completed','rejected') THEN
        RAISE EXCEPTION 'unknown marketing operation can only be resolved by provider readback'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status AND NEW.status = 'started' THEN
        RAISE EXCEPTION 'marketing operation cannot return to started'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := now();
    RETURN NEW;
END;
$$;

COMMIT;
