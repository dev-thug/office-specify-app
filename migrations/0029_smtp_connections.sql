BEGIN;

CREATE TABLE smtp_connections (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    sender_identity_id UUID NOT NULL,
    channel_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    host TEXT NOT NULL CHECK (length(host) BETWEEN 1 AND 253),
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    security TEXT NOT NULL CHECK (security IN ('tls','starttls')),
    username TEXT NOT NULL CHECK (length(username) BETWEEN 1 AND 255),
    from_address TEXT NOT NULL CHECK (length(from_address) BETWEEN 3 AND 320),
    from_name TEXT NOT NULL CHECK (length(btrim(from_name)) BETWEEN 1 AND 300),
    reply_to TEXT,
    custom_headers JSONB NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom_headers) = 'object'),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','active','disabled','reauth_required')),
    last_error_code TEXT,
    has_password BOOLEAN NOT NULL DEFAULT true,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, channel_id),
    FOREIGN KEY (workspace_id, id)
        REFERENCES integration_connections (workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, mail_account_id)
        REFERENCES mail_accounts (workspace_id, id),
    FOREIGN KEY (workspace_id, mail_account_id, sender_identity_id)
        REFERENCES sender_identities (workspace_id, mail_account_id, id),
    FOREIGN KEY (workspace_id, channel_id)
        REFERENCES delivery_channels (workspace_id, id)
);

CREATE INDEX smtp_connections_workspace_status
    ON smtp_connections (workspace_id, status, created_at DESC);

CREATE FUNCTION guard_smtp_connection_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'SMTP connection profiles cannot be deleted'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.id IS DISTINCT FROM OLD.id
       OR NEW.mail_account_id IS DISTINCT FROM OLD.mail_account_id
       OR NEW.sender_identity_id IS DISTINCT FROM OLD.sender_identity_id
       OR NEW.channel_id IS DISTINCT FROM OLD.channel_id
       OR NEW.name IS DISTINCT FROM OLD.name
       OR NEW.host IS DISTINCT FROM OLD.host
       OR NEW.port IS DISTINCT FROM OLD.port
       OR NEW.security IS DISTINCT FROM OLD.security
       OR NEW.username IS DISTINCT FROM OLD.username
       OR NEW.from_address IS DISTINCT FROM OLD.from_address
       OR NEW.from_name IS DISTINCT FROM OLD.from_name
       OR NEW.reply_to IS DISTINCT FROM OLD.reply_to
       OR NEW.custom_headers IS DISTINCT FROM OLD.custom_headers
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'SMTP connection identity and transport settings are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER smtp_connection_immutable
    BEFORE UPDATE OR DELETE ON smtp_connections
    FOR EACH ROW EXECUTE FUNCTION guard_smtp_connection_immutable();

CREATE FUNCTION guard_smtp_connection_binding() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM delivery_channels dc
         WHERE dc.workspace_id = NEW.workspace_id
           AND dc.id = NEW.channel_id
           AND dc.connection_id = NEW.id
           AND dc.mail_account_id = NEW.mail_account_id
           AND dc.sender_identity_id = NEW.sender_identity_id
           AND dc.provider = 'delivery_provider'
           AND dc.purpose = 'marketing'
    ) THEN
        RAISE EXCEPTION 'SMTP connection must bind its own Office marketing channel'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER smtp_connection_binding_guard
    BEFORE INSERT ON smtp_connections
    FOR EACH ROW EXECUTE FUNCTION guard_smtp_connection_binding();

ALTER TABLE smtp_connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE smtp_connections FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON smtp_connections
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

REVOKE ALL ON smtp_connections FROM api_runtime, worker_runtime, report_reader;
GRANT SELECT, INSERT, UPDATE ON smtp_connections TO api_runtime;
GRANT SELECT ON smtp_connections TO worker_runtime;

COMMIT;
