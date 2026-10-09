BEGIN;

-- Personalized values are frozen with the launch, not added after its
-- audience and outbound snapshots have been frozen.
CREATE FUNCTION guard_marketing_launch_values_building() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    launch_state TEXT;
BEGIN
    SELECT status INTO launch_state
      FROM campaign_launches
     WHERE workspace_id = NEW.workspace_id AND id = NEW.launch_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'marketing launch does not exist'
            USING ERRCODE = '23503';
    END IF;
    IF launch_state <> 'building' THEN
        RAISE EXCEPTION 'marketing launch values can only be inserted while the launch is building'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER marketing_launch_values_building_guard
    BEFORE INSERT ON marketing_launch_values
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_launch_values_building();

-- The provider address must be the exact normalized Office address bound to
-- this recipient ID. Keep the existing launch/enrollment/outbound checks.
CREATE OR REPLACE FUNCTION guard_marketing_export_recipient_source() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target_launch UUID;
    target_revision UUID;
    target_step UUID;
    expected_address TEXT;
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

        SELECT normalized_key INTO expected_address
          FROM email_addresses
         WHERE workspace_id = NEW.workspace_id AND id = NEW.email_address_id
         FOR KEY SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'marketing export recipient email address does not exist'
                USING ERRCODE = '23503';
        END IF;
        IF NEW.normalized_address IS DISTINCT FROM expected_address THEN
            RAISE EXCEPTION 'marketing export recipient address must match the normalized Office address'
                USING ERRCODE = '23514';
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

CREATE TABLE marketing_export_reviews (
    workspace_id UUID NOT NULL,
    export_id UUID NOT NULL,
    approval_request_id UUID NOT NULL,
    approver_id UUID NOT NULL,
    export_hash BYTEA NOT NULL CHECK (octet_length(export_hash) = 32),
    reviewed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, export_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id),
    FOREIGN KEY (workspace_id, export_id)
        REFERENCES marketing_exports(workspace_id, id),
    FOREIGN KEY (workspace_id, approval_request_id)
        REFERENCES approval_requests(workspace_id, id),
    FOREIGN KEY (workspace_id, approver_id)
        REFERENCES principals(workspace_id, id)
);

CREATE FUNCTION guard_marketing_export_review_binding() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    export_row marketing_exports%ROWTYPE;
    request_row approval_requests%ROWTYPE;
    decision_exists BOOLEAN;
BEGIN
    SELECT * INTO export_row
      FROM marketing_exports
     WHERE workspace_id = NEW.workspace_id AND id = NEW.export_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'marketing export does not exist'
            USING ERRCODE = '23503';
    END IF;

    SELECT * INTO request_row
      FROM approval_requests
     WHERE workspace_id = NEW.workspace_id AND id = NEW.approval_request_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'approval request does not exist'
            USING ERRCODE = '23503';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM approval_decisions d
          JOIN principals p
            ON p.workspace_id = d.workspace_id AND p.id = d.decided_by
         WHERE d.workspace_id = NEW.workspace_id
           AND d.approval_request_id = request_row.id
           AND d.decision = 'approved'
           AND d.decided_by = NEW.approver_id
           AND d.snapshot_hash = request_row.snapshot_hash
           AND d.version_at_decision = request_row.version - 1
           AND p.kind = 'human'
           AND p.status = 'active'
           AND p.role IN ('admin','manager')
    ) INTO decision_exists;

    IF export_row.state <> 'ready'
       OR export_row.export_hash IS DISTINCT FROM NEW.export_hash
       OR request_row.status <> 'approved'
       OR request_row.action_type <> 'campaign_launch'
       OR request_row.launch_id IS DISTINCT FROM export_row.launch_id
       OR request_row.snapshot_hash IS DISTINCT FROM export_row.source_manifest_hash
       OR request_row.expires_at <= clock_timestamp()
       OR NEW.approver_id = export_row.prepared_by
       OR NEW.approver_id = request_row.requested_by
       OR NOT decision_exists THEN
        RAISE EXCEPTION 'marketing export review must bind a ready export to its current independent approved decision'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER marketing_export_review_binding_guard
    BEFORE INSERT ON marketing_export_reviews
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_export_review_binding();

CREATE FUNCTION guard_marketing_export_review_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'marketing export reviews are append-only'
        USING ERRCODE = '23514';
END;
$$;

CREATE TRIGGER marketing_export_review_append_only
    BEFORE UPDATE OR DELETE ON marketing_export_reviews
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_export_review_append_only();

ALTER TABLE marketing_export_reviews ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketing_export_reviews FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON marketing_export_reviews
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

REVOKE ALL ON marketing_export_reviews FROM api_runtime, worker_runtime, report_reader;
GRANT SELECT, INSERT ON marketing_export_reviews TO api_runtime;
GRANT SELECT ON marketing_export_reviews TO worker_runtime;

COMMIT;
