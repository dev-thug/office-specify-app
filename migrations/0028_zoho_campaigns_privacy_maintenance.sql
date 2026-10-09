BEGIN;

-- The retention role may clear person data from an existing snapshot only
-- after begin_retention_operation has verified the role and set its local
-- transaction flag. API/worker cannot acquire this exception by setting the
-- GUC themselves.
GRANT SELECT, UPDATE ON
    marketing_launch_values, marketing_export_recipients, marketing_exports
TO retention_operator;
GRANT SELECT ON marketing_operations TO retention_operator;

CREATE OR REPLACE FUNCTION guard_marketing_launch_values_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE'
       AND current_user = 'retention_operator'
       AND current_setting('app.retention_operator', true) = 'on' THEN
        IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
           OR NEW.launch_id IS DISTINCT FROM OLD.launch_id
           OR NEW.email_address_id IS DISTINCT FROM OLD.email_address_id
           OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
            RAISE EXCEPTION 'retention may scrub launch values but cannot change their identity'
                USING ERRCODE = '23514';
        END IF;
        IF NEW.variables IS DISTINCT FROM '{}'::jsonb THEN
            RAISE EXCEPTION 'retention may only clear launch values'
                USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'marketing launch values are immutable'
        USING ERRCODE = '23514';
END;
$$;

CREATE OR REPLACE FUNCTION guard_marketing_export_recipient_source() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target_launch UUID;
    target_revision UUID;
    target_step UUID;
    target_state TEXT;
    target_report JSONB;
    target_content_hash BYTEA;
    expected_address TEXT;
BEGIN
    IF TG_OP = 'UPDATE'
       AND current_user = 'retention_operator'
       AND current_setting('app.retention_operator', true) = 'on' THEN
        IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
           OR NEW.export_id IS DISTINCT FROM OLD.export_id
           OR NEW.email_address_id IS DISTINCT FROM OLD.email_address_id
           OR NEW.outbound_message_id IS DISTINCT FROM OLD.outbound_message_id
           OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
            RAISE EXCEPTION 'retention may scrub recipient values but cannot change their identity'
                USING ERRCODE = '23514';
        END IF;
        IF NEW.variables IS DISTINCT FROM '{}'::jsonb
           OR NEW.normalized_address IS DISTINCT FROM 'privacy-erased:' || NEW.email_address_id::text
           OR NEW.sync_status <> 'excluded' THEN
            RAISE EXCEPTION 'retention may only clear and exclude recipient values'
                USING ERRCODE = '23514';
        END IF;
        NEW.updated_at := now();
        RETURN NEW;
    END IF;

    IF TG_OP = 'INSERT' THEN
        SELECT launch_id, revision_id, step_id, state, report, content_hash
          INTO target_launch, target_revision, target_step,
               target_state, target_report, target_content_hash
          FROM marketing_exports
         WHERE workspace_id = NEW.workspace_id AND id = NEW.export_id
         FOR UPDATE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'marketing export does not exist'
                USING ERRCODE = '23503';
        END IF;
        IF target_state <> 'preparing'
           OR target_report->>'privacy_erased' = 'true'
           OR target_content_hash = decode(repeat('00', 32), 'hex')
           OR EXISTS (
                SELECT 1 FROM marketing_operations o
                 WHERE o.workspace_id = NEW.workspace_id
                   AND o.export_id = NEW.export_id
           ) THEN
            RAISE EXCEPTION 'marketing export recipients can only be added before provider operations while preparing'
                USING ERRCODE = '23514';
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

    IF EXISTS (
        SELECT 1 FROM marketing_exports e
         WHERE e.workspace_id = OLD.workspace_id
           AND e.id = OLD.export_id
           AND e.report->>'privacy_erased' = 'true'
    ) THEN
        RAISE EXCEPTION 'privacy-erased marketing export recipients cannot be refreshed'
            USING ERRCODE = '23514';
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

CREATE OR REPLACE FUNCTION guard_marketing_export_snapshot() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'marketing export snapshots are immutable'
            USING ERRCODE = '23514';
    END IF;

    IF current_user = 'retention_operator'
       AND current_setting('app.retention_operator', true) = 'on' THEN
        IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
           OR NEW.id IS DISTINCT FROM OLD.id
           OR NEW.launch_id IS DISTINCT FROM OLD.launch_id
           OR NEW.revision_id IS DISTINCT FROM OLD.revision_id
           OR NEW.step_id IS DISTINCT FROM OLD.step_id
           OR NEW.delivery_channel_id IS DISTINCT FROM OLD.delivery_channel_id
           OR NEW.connection_id IS DISTINCT FROM OLD.connection_id
           OR NEW.provider_list_key IS DISTINCT FROM OLD.provider_list_key
           OR NEW.topic_id IS DISTINCT FROM OLD.topic_id
           OR NEW.campaign_key IS DISTINCT FROM OLD.campaign_key
           OR NEW.sender_address IS DISTINCT FROM OLD.sender_address
           OR NEW.sender_name IS DISTINCT FROM OLD.sender_name
           OR NEW.prepared_by IS DISTINCT FROM OLD.prepared_by
           OR NEW.created_at IS DISTINCT FROM OLD.created_at
           OR NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
            RAISE EXCEPTION 'retention may scrub export content but cannot change its identity or provider IDs'
                USING ERRCODE = '23514';
        END IF;
        IF NEW.state <> 'canceled'
           OR NEW.source_manifest_hash IS DISTINCT FROM decode(repeat('00', 32), 'hex')
           OR NEW.content_hash IS DISTINCT FROM decode(repeat('00', 32), 'hex')
           OR NEW.audience_hash IS DISTINCT FROM decode(repeat('00', 32), 'hex')
           OR NEW.export_hash IS DISTINCT FROM decode(repeat('00', 32), 'hex')
           OR NEW.subject <> '' OR NEW.body_text <> '' OR NEW.body_html <> ''
           OR NEW.field_map IS DISTINCT FROM '{}'::jsonb
           OR NEW.recipient_count <> 0
           OR NOT (NEW.report @> '{"privacy_erased":true,"provider_cancel_not_requested":true}'::jsonb) THEN
            RAISE EXCEPTION 'retention may only cancel and redact a marketing export'
                USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.report->>'privacy_erased' = 'true'
       OR OLD.content_hash = decode(repeat('00', 32), 'hex') THEN
        RAISE EXCEPTION 'privacy-erased marketing exports cannot be refreshed'
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

CREATE FUNCTION guard_marketing_operation_privacy_erased() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    erased BOOLEAN;
BEGIN
    SELECT (report->>'privacy_erased' = 'true'
            OR content_hash = decode(repeat('00', 32), 'hex'))
      INTO erased
      FROM marketing_exports
     WHERE workspace_id = NEW.workspace_id AND id = NEW.export_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'marketing export does not exist'
            USING ERRCODE = '23503';
    END IF;
    IF erased THEN
        RAISE EXCEPTION 'privacy-erased marketing exports cannot start provider operations'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER marketing_operations_privacy_guard
    BEFORE INSERT ON marketing_operations
    FOR EACH ROW EXECUTE FUNCTION guard_marketing_operation_privacy_erased();

COMMIT;
