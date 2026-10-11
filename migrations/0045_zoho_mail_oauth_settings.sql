BEGIN;

-- Workspace-level Zoho Mail OAuth client settings entered in Office Settings.
-- The client secret itself lives only in the encrypted SecretStore under
-- `secret_ref`; this row keeps the non-secret client ID and callback URI.
CREATE TABLE zoho_mail_oauth_settings (
    workspace_id UUID PRIMARY KEY REFERENCES workspaces (id),
    client_id TEXT NOT NULL CHECK (char_length(client_id) BETWEEN 6 AND 200),
    redirect_uri TEXT CHECK (redirect_uri IS NULL OR char_length(redirect_uri) <= 2048),
    secret_ref TEXT NOT NULL CHECK (char_length(secret_ref) BETWEEN 1 AND 300),
    updated_by UUID NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    version BIGINT NOT NULL DEFAULT 1
);

ALTER TABLE zoho_mail_oauth_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE zoho_mail_oauth_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON zoho_mail_oauth_settings
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

REVOKE ALL ON zoho_mail_oauth_settings FROM api_runtime, worker_runtime, report_reader;
GRANT SELECT, INSERT, UPDATE ON zoho_mail_oauth_settings TO api_runtime;
GRANT SELECT ON zoho_mail_oauth_settings TO worker_runtime;

COMMIT;
