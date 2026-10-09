BEGIN;

-- Durable one-way propagation of Office suppressions to each connected
-- Campaigns organization. Unknown/interrupted writes stay terminal so a
-- worker retry can never resubmit a request whose result may have reached Zoho.
CREATE TABLE marketing_suppression_operations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    suppression_id UUID NOT NULL,
    email_address_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    status TEXT NOT NULL DEFAULT 'started'
        CHECK (status IN ('started','completed','unknown','rejected')),
    error_code TEXT CHECK (error_code IS NULL OR error_code ~ '^[A-Za-z0-9_.-]{1,128}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, suppression_id, connection_id),
    FOREIGN KEY (workspace_id, suppression_id)
        REFERENCES suppressions(workspace_id, id),
    FOREIGN KEY (workspace_id, email_address_id)
        REFERENCES email_addresses(workspace_id, id),
    FOREIGN KEY (workspace_id, connection_id)
        REFERENCES marketing_connections(workspace_id, id),
    CHECK ((status = 'started') = (completed_at IS NULL))
);

CREATE FUNCTION guard_marketing_suppression_operation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'marketing suppression operation journal rows are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.suppression_id IS DISTINCT FROM OLD.suppression_id
       OR NEW.email_address_id IS DISTINCT FROM OLD.email_address_id
       OR NEW.connection_id IS DISTINCT FROM OLD.connection_id
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'marketing suppression operation identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.status <> 'started' AND (
        NEW.status IS DISTINCT FROM OLD.status
        OR NEW.error_code IS DISTINCT FROM OLD.error_code
        OR NEW.completed_at IS DISTINCT FROM OLD.completed_at
    ) THEN
        RAISE EXCEPTION 'terminal marketing suppression operation results are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status AND NEW.status = 'started' THEN
        RAISE EXCEPTION 'marketing suppression operation cannot return to started'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := now();
    RETURN NEW;
END;
$$;

CREATE TRIGGER marketing_suppression_operation_guard
    BEFORE UPDATE OR DELETE ON marketing_suppression_operations
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_suppression_operation();

ALTER TABLE marketing_suppression_operations ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketing_suppression_operations FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON marketing_suppression_operations
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

GRANT SELECT ON marketing_suppression_operations TO api_runtime;
GRANT SELECT, INSERT, UPDATE ON marketing_suppression_operations TO worker_runtime;
REVOKE ALL ON marketing_suppression_operations FROM report_reader;

COMMIT;
