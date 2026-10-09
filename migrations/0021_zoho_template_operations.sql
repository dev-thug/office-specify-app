-- Durable write-ahead journal for prepaid Zoho Campaigns Email API v2
-- template mutations. The journal contains keyed request/config fingerprints
-- and response metadata only; it never stores template bodies or credentials.
CREATE TABLE zoho_email_template_operations (
  id UUID PRIMARY KEY,
  workspace_id UUID NOT NULL,
  actor_principal_id UUID NOT NULL,
  endpoint TEXT NOT NULL CHECK (endpoint IN ('create','update','delete')),
  idempotency_key_hmac BYTEA NOT NULL CHECK (octet_length(idempotency_key_hmac)=32),
  request_hmac BYTEA NOT NULL CHECK (octet_length(request_hmac)=32),
  provider_config_hmac BYTEA NOT NULL CHECK (octet_length(provider_config_hmac)=32),
  state TEXT NOT NULL CHECK (state IN ('intent','unknown','completed')),
  template_id TEXT,
  deleted BOOLEAN NOT NULL DEFAULT false,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  completed_at TIMESTAMPTZ,
  UNIQUE (workspace_id,id),
  UNIQUE (workspace_id,actor_principal_id,endpoint,idempotency_key_hmac),
  FOREIGN KEY (workspace_id,actor_principal_id) REFERENCES principals(workspace_id,id),
  CHECK ((state='completed') = (completed_at IS NOT NULL)),
  CHECK (NOT deleted OR state='completed')
);

ALTER TABLE zoho_email_template_operations ENABLE ROW LEVEL SECURITY;
ALTER TABLE zoho_email_template_operations FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON zoho_email_template_operations
  USING (workspace_id=app_workspace_id())
  WITH CHECK (workspace_id=app_workspace_id());

GRANT SELECT,INSERT,UPDATE,DELETE ON zoho_email_template_operations TO api_runtime;
REVOKE ALL ON zoho_email_template_operations FROM worker_runtime,report_reader;
