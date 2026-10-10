BEGIN;

-- Keep SMTP identity/channel/profile binding declarative for every
-- personalized campaign created from the workspace default.
ALTER TABLE smtp_connections
    ADD CONSTRAINT smtp_connections_sales_binding_uniq
    UNIQUE (workspace_id, id, mail_account_id, sender_identity_id, channel_id);
ALTER TABLE message_drafts
    ADD CONSTRAINT message_drafts_sales_mailbox_uniq
    UNIQUE (workspace_id, mail_account_id, id);

-- Workspace-wide sender choice. Existing campaigns copy this binding at
-- creation time and therefore are not retargeted by later default changes.
CREATE TABLE sales_campaign_sender_defaults (
    workspace_id UUID PRIMARY KEY,
    smtp_profile_id UUID NOT NULL,
    updated_by UUID NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (workspace_id) REFERENCES workspaces (id),
    FOREIGN KEY (workspace_id, smtp_profile_id)
        REFERENCES smtp_connections (workspace_id, id),
    FOREIGN KEY (workspace_id, updated_by)
        REFERENCES principals (workspace_id, id)
);

CREATE TABLE sales_campaigns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    objective TEXT CHECK (objective IS NULL OR length(objective) <= 10000),
    mail_account_id UUID NOT NULL,
    delivery_channel_id UUID NOT NULL,
    sender_identity_id UUID NOT NULL,
    smtp_profile_id UUID NOT NULL,
    meeting_type_id UUID,
    owner_principal_id UUID NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, id, mail_account_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces (id),
    FOREIGN KEY (workspace_id, mail_account_id, delivery_channel_id)
        REFERENCES delivery_channels (workspace_id, mail_account_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, delivery_channel_id, sender_identity_id)
        REFERENCES delivery_channels (workspace_id, id, sender_identity_id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, smtp_profile_id, mail_account_id,
                 sender_identity_id, delivery_channel_id)
        REFERENCES smtp_connections
            (workspace_id, id, mail_account_id, sender_identity_id, channel_id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, meeting_type_id)
        REFERENCES meeting_types (workspace_id, id) ON DELETE SET NULL (meeting_type_id),
    FOREIGN KEY (workspace_id, owner_principal_id)
        REFERENCES principals (workspace_id, id) ON DELETE RESTRICT
);

-- Composite keys below allow campaign targets to follow CRM company edits
-- while remaining workspace-scoped. Erasure removes only the personal link.
ALTER TABLE contacts
    ADD CONSTRAINT contacts_sales_binding_uniq
    UNIQUE (workspace_id, id, company_id);
ALTER TABLE contact_emails
    ADD CONSTRAINT contact_emails_sales_binding_uniq
    UNIQUE (workspace_id, id, contact_id),
    ADD CONSTRAINT contact_emails_sales_address_uniq
    UNIQUE (workspace_id, id, email_address_id);

CREATE TABLE sales_campaign_targets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    sales_campaign_id UUID NOT NULL,
    company_id UUID,
    contact_id UUID,
    contact_email_id UUID,
    email_address_id UUID,
    workflow_stage TEXT NOT NULL DEFAULT 'research'
        CHECK (workflow_stage IN ('research','draft','review')),
    current_draft_id UUID,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, sales_campaign_id, id),
    UNIQUE (workspace_id, sales_campaign_id, email_address_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces (id),
    FOREIGN KEY (workspace_id, sales_campaign_id)
        REFERENCES sales_campaigns (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, company_id)
        REFERENCES companies (workspace_id, id) ON DELETE SET NULL (company_id),
    FOREIGN KEY (workspace_id, contact_id, company_id)
        REFERENCES contacts (workspace_id, id, company_id)
        ON UPDATE CASCADE ON DELETE SET NULL (contact_id),
    FOREIGN KEY (workspace_id, contact_email_id, contact_id)
        REFERENCES contact_emails (workspace_id, id, contact_id)
        ON DELETE SET NULL (contact_email_id),
    FOREIGN KEY (workspace_id, contact_email_id, email_address_id)
        REFERENCES contact_emails (workspace_id, id, email_address_id)
        ON DELETE SET NULL (contact_email_id, email_address_id),
    FOREIGN KEY (workspace_id, email_address_id)
        REFERENCES email_addresses (workspace_id, id)
        ON DELETE SET NULL (email_address_id)
);

CREATE TABLE sales_campaign_target_drafts (
    workspace_id UUID NOT NULL,
    sales_campaign_id UUID NOT NULL,
    target_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    draft_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, target_id, draft_id),
    UNIQUE (workspace_id, draft_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces (id),
    FOREIGN KEY (workspace_id, sales_campaign_id, mail_account_id)
        REFERENCES sales_campaigns (workspace_id, id, mail_account_id)
        ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, sales_campaign_id, target_id)
        REFERENCES sales_campaign_targets (workspace_id, sales_campaign_id, id)
        ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, mail_account_id, draft_id)
        REFERENCES message_drafts (workspace_id, mail_account_id, id)
        ON DELETE CASCADE
);

ALTER TABLE sales_campaign_targets
    ADD CONSTRAINT sales_campaign_targets_current_draft_fk
    FOREIGN KEY (workspace_id, id, current_draft_id)
    REFERENCES sales_campaign_target_drafts (workspace_id, target_id, draft_id)
    ON DELETE SET NULL (current_draft_id);

CREATE INDEX sales_campaigns_workspace_updated_idx
    ON sales_campaigns (workspace_id, updated_at DESC, id DESC);
CREATE INDEX sales_campaign_targets_campaign_created_idx
    ON sales_campaign_targets (workspace_id, sales_campaign_id, created_at, id);
CREATE INDEX sales_campaign_target_drafts_draft_idx
    ON sales_campaign_target_drafts (workspace_id, draft_id);

ALTER TABLE sales_campaign_sender_defaults ENABLE ROW LEVEL SECURITY;
ALTER TABLE sales_campaign_sender_defaults FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON sales_campaign_sender_defaults
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

ALTER TABLE sales_campaigns ENABLE ROW LEVEL SECURITY;
ALTER TABLE sales_campaigns FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON sales_campaigns
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

ALTER TABLE sales_campaign_targets ENABLE ROW LEVEL SECURITY;
ALTER TABLE sales_campaign_targets FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON sales_campaign_targets
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

ALTER TABLE sales_campaign_target_drafts ENABLE ROW LEVEL SECURITY;
ALTER TABLE sales_campaign_target_drafts FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON sales_campaign_target_drafts
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

REVOKE ALL ON sales_campaign_sender_defaults, sales_campaigns,
    sales_campaign_targets, sales_campaign_target_drafts
    FROM api_runtime, worker_runtime, report_reader, retention_operator;
GRANT SELECT, INSERT, UPDATE, DELETE ON sales_campaign_sender_defaults,
    sales_campaigns, sales_campaign_targets, sales_campaign_target_drafts
    TO api_runtime;
GRANT SELECT ON sales_campaigns, sales_campaign_targets,
    sales_campaign_target_drafts TO worker_runtime;
-- Contact erasure can SET NULL on target pointers through the retained CRM
-- link foreign keys without erasing messages owned by other contacts.
GRANT SELECT, UPDATE ON sales_campaign_targets TO retention_operator;
GRANT SELECT ON sales_campaign_target_drafts TO retention_operator;
-- Erasure may revoke cached sales mutation bodies containing the affected
-- recipient's address/content and cancel pending dispatches for its drafts.
GRANT SELECT, DELETE ON idempotency_records TO retention_operator;
GRANT UPDATE ON jobs TO retention_operator;

COMMIT;
