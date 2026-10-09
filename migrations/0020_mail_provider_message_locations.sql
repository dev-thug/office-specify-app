-- Provider folder coordinates contain no addresses or message content.
CREATE TABLE mail_provider_message_locations (
  workspace_id UUID NOT NULL,
  mail_account_id UUID NOT NULL,
  provider_message_id TEXT NOT NULL CHECK(provider_message_id ~ '^[0-9]{1,64}$'),
  provider_folder_id TEXT NOT NULL CHECK(provider_folder_id ~ '^[0-9]{1,64}$'),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY(workspace_id,mail_account_id,provider_message_id),
  FOREIGN KEY(workspace_id,mail_account_id) REFERENCES mail_accounts(workspace_id,id)
);
ALTER TABLE mail_provider_message_locations ENABLE ROW LEVEL SECURITY;
ALTER TABLE mail_provider_message_locations FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON mail_provider_message_locations USING(workspace_id=app_workspace_id()) WITH CHECK(workspace_id=app_workspace_id());
GRANT SELECT,INSERT,UPDATE ON mail_provider_message_locations TO api_runtime,worker_runtime;
REVOKE ALL ON mail_provider_message_locations FROM report_reader;
