BEGIN;

-- Profile photo per principal (members and agents), shown in the app and as
-- the host picture on public booking pages. The browser downsizes uploads, so
-- rows stay small enough to live in the database instead of the object store.
CREATE TABLE principal_avatars (
    workspace_id UUID NOT NULL,
    principal_id UUID NOT NULL,
    content_type TEXT NOT NULL
        CHECK (content_type IN ('image/png', 'image/jpeg', 'image/webp')),
    bytes BYTEA NOT NULL CHECK (octet_length(bytes) BETWEEN 1 AND 524288),
    updated_by UUID NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, principal_id),
    FOREIGN KEY (workspace_id, principal_id) REFERENCES principals (workspace_id, id) ON DELETE CASCADE
);

ALTER TABLE principal_avatars ENABLE ROW LEVEL SECURITY;
ALTER TABLE principal_avatars FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON principal_avatars
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

REVOKE ALL ON principal_avatars FROM api_runtime, worker_runtime, report_reader;
GRANT SELECT, INSERT, UPDATE, DELETE ON principal_avatars TO api_runtime;

COMMIT;
