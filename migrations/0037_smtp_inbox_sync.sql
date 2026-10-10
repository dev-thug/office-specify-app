BEGIN;

-- IMAP is an explicit, separately-audited opt-in attached to an existing
-- SMTP connection. No credential is copied here: workers resolve the
-- profile's existing integration secret reference.
CREATE TABLE smtp_inbox_monitors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    smtp_connection_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT false,
    imap_host TEXT NOT NULL DEFAULT '' CHECK (length(imap_host) <= 253 AND imap_host = btrim(imap_host)),
    cursor_uidvalidity BIGINT CHECK (cursor_uidvalidity BETWEEN 1 AND 4294967295),
    cursor_uid BIGINT NOT NULL DEFAULT 0 CHECK (cursor_uid BETWEEN 0 AND 4294967295),
    imported_messages BIGINT NOT NULL DEFAULT 0 CHECK (imported_messages >= 0),
    last_success_at TIMESTAMPTZ,
    last_error_code TEXT CHECK (last_error_code IS NULL OR last_error_code ~ '^[A-Z0-9_]{1,80}$'),
    next_sync_at TIMESTAMPTZ,
    active_job_id UUID,
    scheduled_job_id UUID,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, smtp_connection_id),
    FOREIGN KEY (workspace_id, smtp_connection_id)
        REFERENCES smtp_connections (workspace_id, id),
    FOREIGN KEY (workspace_id, mail_account_id)
        REFERENCES mail_accounts (workspace_id, id),
    FOREIGN KEY (workspace_id, active_job_id)
        REFERENCES jobs (workspace_id, id),
    FOREIGN KEY (workspace_id, scheduled_job_id)
        REFERENCES jobs (workspace_id, id),
    CHECK (cursor_uidvalidity IS NOT NULL OR cursor_uid = 0),
    CHECK (enabled OR (active_job_id IS NULL AND scheduled_job_id IS NULL AND next_sync_at IS NULL)),
    CHECK (active_job_id IS NULL OR scheduled_job_id IS NULL),
    CHECK (NOT enabled OR length(imap_host) BETWEEN 1 AND 253)
);

-- A mailbox can have only one active IMAP cursor even if multiple SMTP
-- sender profiles happen to reference that mailbox.
CREATE UNIQUE INDEX smtp_inbox_one_enabled_mailbox
    ON smtp_inbox_monitors (workspace_id, mail_account_id)
    WHERE enabled;

CREATE INDEX smtp_inbox_due_syncs
    ON smtp_inbox_monitors (next_sync_at, id)
    WHERE enabled AND next_sync_at IS NOT NULL;

CREATE FUNCTION guard_smtp_inbox_monitor_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'SMTP inbox monitor history cannot be deleted'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.id IS DISTINCT FROM OLD.id
       OR NEW.smtp_connection_id IS DISTINCT FROM OLD.smtp_connection_id
       OR NEW.mail_account_id IS DISTINCT FROM OLD.mail_account_id
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'SMTP inbox monitor identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := now();
    RETURN NEW;
END;
$$;

CREATE TRIGGER smtp_inbox_monitor_identity_guard
    BEFORE UPDATE OR DELETE ON smtp_inbox_monitors
    FOR EACH ROW EXECUTE FUNCTION guard_smtp_inbox_monitor_identity();

-- Disabling or requiring reconnection for the SMTP profile also ends its
-- inbox polling opt-in and cancels jobs which have not started. A running
-- page is serialized by the monitor row lock and remains lease-fenced.
CREATE FUNCTION stop_smtp_inbox_monitor_for_disabled_profile() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.status IN ('disabled', 'reauth_required') OR NOT NEW.has_password THEN
        UPDATE jobs
           SET state = 'canceled', finished_at = now(),
               last_error_code = 'SMTP_PROFILE_DISABLED',
               lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
         WHERE workspace_id = NEW.workspace_id
           AND kind = 'smtp_inbox.poll'
           AND payload ->> 'monitor_id' IN (
                SELECT id::text FROM smtp_inbox_monitors
                 WHERE workspace_id = NEW.workspace_id
                   AND smtp_connection_id = NEW.id
           )
           AND state IN ('queued', 'retry_wait');

        UPDATE smtp_inbox_monitors
           SET enabled = false,
               last_error_code = 'SMTP_PROFILE_DISABLED',
               active_job_id = NULL,
               scheduled_job_id = NULL,
               next_sync_at = NULL,
               version = version + 1
         WHERE workspace_id = NEW.workspace_id
           AND smtp_connection_id = NEW.id
           AND enabled;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER stop_smtp_inbox_monitor_after_profile_disable
    AFTER UPDATE OF status, has_password ON smtp_connections
    FOR EACH ROW EXECUTE FUNCTION stop_smtp_inbox_monitor_for_disabled_profile();

ALTER TABLE smtp_inbox_monitors ENABLE ROW LEVEL SECURITY;
ALTER TABLE smtp_inbox_monitors FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON smtp_inbox_monitors
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

REVOKE ALL ON smtp_inbox_monitors FROM api_runtime, worker_runtime, report_reader;
GRANT SELECT, INSERT, UPDATE ON smtp_inbox_monitors TO api_runtime, worker_runtime;

COMMIT;
