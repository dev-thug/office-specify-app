BEGIN;

-- Frozen per-recipient merge values from the Office launch. Values are
-- append-only and can only be read by the API and worker roles.
CREATE TABLE marketing_launch_values (
    workspace_id UUID NOT NULL,
    launch_id UUID NOT NULL,
    email_address_id UUID NOT NULL,
    variables JSONB NOT NULL CHECK (jsonb_typeof(variables) = 'object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, launch_id, email_address_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id),
    FOREIGN KEY (workspace_id, launch_id, email_address_id)
        REFERENCES campaign_enrollments(workspace_id, launch_id, email_address_id)
);

CREATE FUNCTION guard_marketing_launch_values_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'marketing launch values are immutable'
        USING ERRCODE = '23514';
END;
$$;

CREATE TRIGGER marketing_launch_values_immutable
    BEFORE UPDATE OR DELETE ON marketing_launch_values
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_launch_values_immutable();

-- Add the channel/connection composite key needed to ensure an export cannot
-- combine a valid Campaigns channel with a different OAuth connection.
ALTER TABLE delivery_channels
    ADD CONSTRAINT delivery_channels_workspace_channel_connection_uq
    UNIQUE (workspace_id, id, connection_id);

CREATE TABLE marketing_exports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    launch_id UUID NOT NULL,
    revision_id UUID NOT NULL,
    step_id UUID NOT NULL,
    delivery_channel_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    source_manifest_hash BYTEA NOT NULL CHECK (octet_length(source_manifest_hash) = 32),
    content_hash BYTEA NOT NULL CHECK (octet_length(content_hash) = 32),
    audience_hash BYTEA NOT NULL CHECK (octet_length(audience_hash) = 32),
    export_hash BYTEA NOT NULL CHECK (octet_length(export_hash) = 32),
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL,
    body_html TEXT NOT NULL,
    sender_address TEXT NOT NULL CHECK (length(btrim(sender_address)) > 0),
    sender_name TEXT NOT NULL CHECK (length(btrim(sender_name)) > 0),
    field_map JSONB NOT NULL CHECK (jsonb_typeof(field_map) = 'object'),
    provider_list_key TEXT,
    topic_id TEXT,
    campaign_key TEXT,
    state TEXT NOT NULL DEFAULT 'preparing'
        CHECK (state IN ('preparing','syncing','needs_review','ready','submitting',
                         'in_progress','completed','rejected','unknown','canceled')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    prepared_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_checked_at TIMESTAMPTZ,
    report JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(report) = 'object'),
    recipient_count BIGINT NOT NULL DEFAULT 0 CHECK (recipient_count >= 0),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, launch_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id),
    FOREIGN KEY (workspace_id, revision_id, launch_id)
        REFERENCES campaign_launches(workspace_id, revision_id, id),
    FOREIGN KEY (workspace_id, launch_id, delivery_channel_id)
        REFERENCES campaign_launches(workspace_id, id, delivery_channel_id),
    FOREIGN KEY (workspace_id, revision_id, step_id)
        REFERENCES campaign_steps(workspace_id, revision_id, id),
    FOREIGN KEY (workspace_id, delivery_channel_id, connection_id)
        REFERENCES delivery_channels(workspace_id, id, connection_id),
    FOREIGN KEY (workspace_id, connection_id)
        REFERENCES integration_connections(workspace_id, id),
    FOREIGN KEY (workspace_id, prepared_by)
        REFERENCES principals(workspace_id, id)
);

CREATE TABLE marketing_export_recipients (
    workspace_id UUID NOT NULL,
    export_id UUID NOT NULL,
    email_address_id UUID NOT NULL,
    outbound_message_id UUID NOT NULL,
    normalized_address TEXT NOT NULL CHECK (length(btrim(normalized_address)) BETWEEN 3 AND 320),
    variables JSONB NOT NULL CHECK (jsonb_typeof(variables) = 'object'),
    sync_status TEXT NOT NULL DEFAULT 'pending'
        CHECK (sync_status IN ('pending','syncing','active','pending_confirmation',
                               'unsubscribed','bounced','excluded','unknown')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, export_id, email_address_id),
    UNIQUE (workspace_id, export_id, normalized_address),
    FOREIGN KEY (workspace_id, export_id)
        REFERENCES marketing_exports(workspace_id, id),
    FOREIGN KEY (workspace_id, email_address_id, outbound_message_id)
        REFERENCES outbound_messages(workspace_id, to_email_address_id, id)
);

-- The ordinary outbound FKs prove workspace and address identity. This
-- trigger also pins it to the export's launch, revision, step, and matching
-- launch enrollment before any recipient can enter the provider snapshot.
CREATE FUNCTION guard_marketing_export_recipient_source() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target_launch UUID;
    target_revision UUID;
    target_step UUID;
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT launch_id, revision_id, step_id
          INTO target_launch, target_revision, target_step
          FROM marketing_exports
         WHERE workspace_id = NEW.workspace_id AND id = NEW.export_id;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'marketing export does not exist'
                USING ERRCODE = '23503';
        END IF;

        IF NOT EXISTS (
            SELECT 1
              FROM outbound_messages o
              JOIN campaign_enrollments e
                ON e.workspace_id = o.workspace_id
               AND e.launch_id = o.launch_id
               AND e.revision_id = o.revision_id
               AND e.id = o.enrollment_id
               AND e.email_address_id = o.to_email_address_id
             WHERE o.workspace_id = NEW.workspace_id
               AND o.id = NEW.outbound_message_id
               AND o.to_email_address_id = NEW.email_address_id
               AND o.launch_id = target_launch
               AND o.revision_id = target_revision
               AND o.step_id = target_step
        ) THEN
            RAISE EXCEPTION 'marketing export recipient source outbound does not match launch, step, and address'
                USING ERRCODE = '23503';
        END IF;
        RETURN NEW;
    END IF;

    IF TG_OP = 'DELETE' THEN
        IF NOT EXISTS (
            SELECT 1 FROM marketing_exports e
             WHERE e.workspace_id = OLD.workspace_id
               AND e.id = OLD.export_id
               AND e.state = 'preparing'
               AND NOT EXISTS (
                   SELECT 1 FROM marketing_operations o
                    WHERE o.workspace_id = e.workspace_id AND o.export_id = e.id
               )
        ) THEN
            RAISE EXCEPTION 'marketing export recipients can only be replaced before provider operations'
                USING ERRCODE = '23514';
        END IF;
        RETURN OLD;
    END IF;

    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.export_id IS DISTINCT FROM OLD.export_id
       OR NEW.email_address_id IS DISTINCT FROM OLD.email_address_id
       OR NEW.outbound_message_id IS DISTINCT FROM OLD.outbound_message_id
       OR NEW.normalized_address IS DISTINCT FROM OLD.normalized_address
       OR NEW.variables IS DISTINCT FROM OLD.variables
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'marketing export recipient snapshot is immutable'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := now();
    RETURN NEW;
END;
$$;

CREATE TRIGGER marketing_export_recipient_source_guard
    BEFORE INSERT OR UPDATE OR DELETE ON marketing_export_recipients
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_export_recipient_source();

CREATE TABLE marketing_operations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    export_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    correlation_key TEXT NOT NULL CHECK (length(btrim(correlation_key)) BETWEEN 1 AND 200),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    status TEXT NOT NULL DEFAULT 'started'
        CHECK (status IN ('started','completed','unknown','rejected')),
    provider_id TEXT,
    error_code TEXT CHECK (error_code IS NULL OR error_code ~ '^[A-Za-z0-9_.-]{1,128}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, export_id, kind, correlation_key),
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id),
    FOREIGN KEY (workspace_id, export_id)
        REFERENCES marketing_exports(workspace_id, id),
    CHECK ((status = 'started') = (completed_at IS NULL))
);

CREATE FUNCTION guard_marketing_export_snapshot() RETURNS trigger
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
           OR (OLD.state = 'needs_review' AND NEW.state = 'canceled')
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

CREATE TRIGGER marketing_export_snapshot_guard
    BEFORE UPDATE OR DELETE ON marketing_exports
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_export_snapshot();

CREATE FUNCTION guard_marketing_operation() RETURNS trigger
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
    IF OLD.status <> 'started' AND (
        NEW.status IS DISTINCT FROM OLD.status
        OR NEW.provider_id IS DISTINCT FROM OLD.provider_id
        OR NEW.error_code IS DISTINCT FROM OLD.error_code
        OR NEW.completed_at IS DISTINCT FROM OLD.completed_at
    ) THEN
        RAISE EXCEPTION 'terminal marketing operation results are immutable'
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

CREATE TRIGGER marketing_operation_guard
    BEFORE UPDATE OR DELETE ON marketing_operations
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_operation();

ALTER TABLE marketing_launch_values ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketing_launch_values FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON marketing_launch_values
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

ALTER TABLE marketing_exports ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketing_exports FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON marketing_exports
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

ALTER TABLE marketing_export_recipients ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketing_export_recipients FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON marketing_export_recipients
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

ALTER TABLE marketing_operations ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketing_operations FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON marketing_operations
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

GRANT SELECT, INSERT ON marketing_launch_values TO api_runtime, worker_runtime;
GRANT SELECT, INSERT, UPDATE ON
    marketing_exports, marketing_export_recipients, marketing_operations
    TO api_runtime, worker_runtime;
REVOKE ALL ON
    marketing_launch_values, marketing_exports,
    marketing_export_recipients, marketing_operations
    FROM report_reader;

COMMIT;
