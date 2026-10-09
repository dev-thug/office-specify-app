-- One-time browser/session ownership for real Mail OAuth callbacks.
CREATE TABLE integration_oauth_requests (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID NOT NULL,
  connection_id UUID NOT NULL,
  principal_id UUID NOT NULL,
  session_id UUID NOT NULL,
  state_hash BYTEA NOT NULL UNIQUE CHECK(octet_length(state_hash)=32),
  expires_at TIMESTAMPTZ NOT NULL,
  used_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(workspace_id,id),
  FOREIGN KEY(workspace_id,connection_id) REFERENCES integration_connections(workspace_id,id),
  FOREIGN KEY(workspace_id,principal_id) REFERENCES principals(workspace_id,id),
  FOREIGN KEY(workspace_id,session_id) REFERENCES user_sessions(workspace_id,id)
);
ALTER TABLE integration_oauth_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE integration_oauth_requests FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON integration_oauth_requests USING(workspace_id=app_workspace_id()) WITH CHECK(workspace_id=app_workspace_id());
GRANT SELECT,INSERT,UPDATE ON integration_oauth_requests TO api_runtime;
REVOKE ALL ON integration_oauth_requests FROM worker_runtime,report_reader;
