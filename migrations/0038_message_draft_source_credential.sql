BEGIN;

-- Bind an agent-authored draft to the active service credential that created
-- it. A human may freeze and approve the snapshot later, but approved-submit
-- remains limited to this original credential and stops after revocation.
ALTER TABLE message_drafts
    ADD COLUMN source_credential_id UUID;

ALTER TABLE message_drafts
    ADD CONSTRAINT message_drafts_source_credential_fk
    FOREIGN KEY (workspace_id, source_credential_id)
    REFERENCES service_credentials (workspace_id, id);

COMMIT;
