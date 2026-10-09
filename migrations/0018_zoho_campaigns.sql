-- Zoho Campaigns uses a distinct marketing-only provider identity.
ALTER TABLE integration_connections DROP CONSTRAINT integration_connections_provider_check;
ALTER TABLE integration_connections ADD CONSTRAINT integration_connections_provider_check
CHECK (provider IN ('zoho_mail','zoho_calendar','delivery_provider','transactional_provider','zoho_campaigns'));
ALTER TABLE delivery_channels DROP CONSTRAINT delivery_channels_provider_check;
ALTER TABLE delivery_channels ADD CONSTRAINT delivery_channels_provider_check
CHECK (provider IN ('zoho_mail','delivery_provider','transactional_provider','zoho_campaigns'));
ALTER TABLE delivery_channels ADD CONSTRAINT zoho_campaigns_marketing_only
CHECK (provider <> 'zoho_campaigns' OR purpose='marketing');

CREATE TABLE marketing_connections (
  id UUID PRIMARY KEY,
  workspace_id UUID NOT NULL,
  name TEXT NOT NULL DEFAULT 'Zoho Campaigns',
  plan TEXT NOT NULL DEFAULT 'free' CHECK (plan='free'),
  last_verified_at TIMESTAMPTZ,
  lists_count INTEGER NOT NULL DEFAULT 0 CHECK (lists_count >= 0),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(workspace_id,id),
  FOREIGN KEY(workspace_id,id) REFERENCES integration_connections(workspace_id,id)
);
CREATE TABLE marketing_oauth_states (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID NOT NULL,
  connection_id UUID NOT NULL,
  principal_id UUID NOT NULL,
  session_id UUID NOT NULL,
  state_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(state_hash)=32),
  redirect_uri TEXT NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  used_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(workspace_id,id),
  FOREIGN KEY(workspace_id,connection_id) REFERENCES marketing_connections(workspace_id,id),
  FOREIGN KEY(workspace_id,principal_id) REFERENCES principals(workspace_id,id),
  FOREIGN KEY(workspace_id,session_id) REFERENCES user_sessions(workspace_id,id)
);
ALTER TABLE marketing_connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketing_connections FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON marketing_connections USING(workspace_id=app_workspace_id()) WITH CHECK(workspace_id=app_workspace_id());
ALTER TABLE marketing_oauth_states ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketing_oauth_states FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON marketing_oauth_states USING(workspace_id=app_workspace_id()) WITH CHECK(workspace_id=app_workspace_id());
GRANT SELECT,INSERT,UPDATE ON marketing_connections,marketing_oauth_states TO api_runtime;
GRANT SELECT,UPDATE ON marketing_connections TO worker_runtime;
REVOKE ALL ON marketing_connections,marketing_oauth_states FROM report_reader;
